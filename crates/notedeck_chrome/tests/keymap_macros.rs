//! Macros through real chrome frames: a key bound to a `[macro.<name>]` runs
//! its first step as the key fires and each later one once the step before it
//! has settled (`notedeck_chrome::keymap::macros`), and aborts a step that
//! doesn't settle in time.
//!
//! The first tests drive the fake app of `common` with test keymaps. The last
//! ones run the shipped `E` macro: a stand-in for Dave, in front, answers
//! `dave_open_issue` with the session's card ref as real Dave does
//! (`notedeck_dave`'s `open_issue_hands_chrome_the_sessions_issue` checks
//! that half), and the real Headway, on a seeded board, opens the card and its
//! explainer.

mod common;

use common::Fixture;
use egui::{Key, Modifiers};
use notedeck_chrome::keymap::macros::MACRO_TIMEOUT_SECS;
use serde_json::json;

/// The fake app's scope, with `m` bound to the macro `two` and `n` to `gated`.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "m"
macro = "two"

[[bind]]
scope = "fake"
keys = "n"
macro = "gated"

[[bind]]
scope = "fake"
keys = "o"
macro = "gated-first"

[[bind]]
scope = "fake"
keys = "u"
macro = "unknown"

[[bind]]
scope = "fake"
keys = "c"
macro = "mark-current"

[[bind]]
scope = "fake"
keys = "x"
macro = "current-first"

[[bind]]
scope = "global"
keys = "?"
tool = "chrome_toggle_key_hints"

[macro.two]
label = "down then first"
steps = [ { tool = "fake_down" }, { tool = "fake_first" } ]

[macro.gated]
steps = [ { tool = "fake_down" }, { tool = "fake_gated" } ]

[macro.gated-first]
steps = [ { tool = "fake_gated" }, { tool = "fake_down" } ]

[macro.unknown]
steps = [ { tool = "fake_down" }, { tool = "no_such_tool" } ]

[macro.mark-current]
steps = [ { tool = "fake_down" }, { tool = "fake_mark", args = { card = "{current}", column = "done" } } ]

[macro.current-first]
steps = [ { tool = "fake_mark", args = { card = "{current}" } }, { tool = "fake_down" } ]
"#;

/// The first step runs as the key fires; the second waits for a frame to
/// render with the first settled, then runs, and the macro is done.
#[test]
fn a_macro_runs_its_steps_on_successive_frames() {
    let mut f = Fixture::new(KEYMAP);
    f.key(Key::M);
    assert_eq!(f.calls(), ["fake_down"], "step 1 runs as the key fires");
    assert_eq!(f.chrome.running_macro(), Some("two"));
    assert!(!f.seen().contains(&Key::M), "the key is the macro's");

    f.frame(Vec::new());
    assert_eq!(f.calls(), ["fake_down", "fake_first"]);
    assert_eq!(f.chrome.running_macro(), None, "done");
}

/// A step whose tool never becomes available aborts the macro once its wait
/// runs out, leaving the steps before it done and nothing after.
#[test]
fn a_step_that_never_settles_aborts_after_the_timeout() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    f.key(Key::N);
    assert_eq!(f.calls(), ["fake_down"]);

    f.idle(2.8);
    assert_eq!(f.chrome.running_macro(), Some("gated"), "still waiting");
    f.idle(0.3);
    assert_eq!(f.chrome.running_macro(), None, "aborted");
    assert_eq!(f.calls(), ["fake_down"], "the unsettled step never ran");

    // Becoming available later doesn't revive it.
    f.state.borrow_mut().gated_available = true;
    f.idle(0.1);
    assert_eq!(f.calls(), ["fake_down"]);
}

/// A step that settles inside the wait runs.
#[test]
fn a_step_runs_once_its_tool_becomes_available() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    f.key(Key::N);
    f.idle(1.0);
    assert_eq!(f.calls(), ["fake_down"]);

    f.state.borrow_mut().gated_available = true;
    f.idle(0.1);
    assert_eq!(f.calls(), ["fake_down", "fake_gated"]);
    assert_eq!(f.chrome.running_macro(), None);
}

/// A macro whose first step's tool isn't available doesn't fire: its key
/// falls through to the app.
#[test]
fn a_macro_whose_first_step_is_unavailable_falls_through() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    f.key(Key::O);
    assert!(f.calls().is_empty());
    assert_eq!(f.chrome.running_macro(), None);
    assert!(f.seen().contains(&Key::O), "the app gets the key");
}

/// A macro naming a tool no app has is reported when the keymap is checked,
/// and running it aborts before its first step.
#[test]
fn an_unknown_tool_is_reported_and_aborts_at_once() {
    let mut f = Fixture::new(KEYMAP);
    let problems = f
        .driver
        .chrome
        .keymap_problems(&f.driver.notedeck.app_context());
    assert_eq!(
        problems,
        ["macro 'unknown' step 2: no app has a tool 'no_such_tool'"]
    );

    f.key(Key::U);
    assert!(f.calls().is_empty(), "no step runs");
    assert_eq!(f.chrome.running_macro(), None);
}

/// A macro key pressed while another macro waits replaces it.
#[test]
fn a_new_macro_replaces_a_running_one() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    f.key(Key::N);
    assert_eq!(f.chrome.running_macro(), Some("gated"));

    f.key(Key::M);
    assert_eq!(f.chrome.running_macro(), Some("two"));
    f.frame(Vec::new());
    assert_eq!(f.calls(), ["fake_down", "fake_down", "fake_first"]);

    // The replaced run is gone for good.
    f.state.borrow_mut().gated_available = true;
    f.idle(0.2);
    assert_eq!(f.calls(), ["fake_down", "fake_down", "fake_first"]);
}

/// A frame with a macro waiting doesn't stall: the wait asks for a repaint,
/// so an idle window still gets the frames that run the next step.
#[test]
fn a_waiting_macro_asks_for_frames() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    f.key(Key::N);
    assert!(
        f.egui_ctx.has_requested_repaint(),
        "a waiting macro schedules the next check"
    );
}

/// A step naming `{current}` runs on the current ref of the app in front as
/// the step runs, not as the key fired: the step before it is what moved
/// the app onto it (Dave's `dave_open_issue`, then Headway's card).
#[tokio::test]
async fn a_step_runs_on_the_current_ref_when_it_runs() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().current = Some("fake:before".to_owned());
    f.frame(Vec::new());
    f.key(Key::C);
    assert_eq!(f.calls(), ["fake_down"]);

    // What the first step navigated to becomes current before the second runs.
    f.state.borrow_mut().current = Some("fake:after".to_owned());
    f.idle(0.1);
    assert_eq!(
        f.marked(),
        [json!({ "card": "fake:after", "column": "done" })]
    );
    assert_eq!(f.chrome.running_macro(), None, "done");
}

/// A step naming `{current}` waits for the app in front to have one, and
/// aborts if it never does; it never runs on the literal `{current}`.
#[tokio::test]
async fn a_step_naming_current_waits_for_one() {
    let mut f = Fixture::new(KEYMAP);
    f.key(Key::C);
    f.idle(1.0);
    assert_eq!(f.marked(), Vec::<serde_json::Value>::new(), "still waiting");
    assert_eq!(f.chrome.running_macro(), Some("mark-current"));

    f.state.borrow_mut().current = Some("fake:late".to_owned());
    f.idle(0.1);
    assert_eq!(
        f.marked(),
        [json!({ "card": "fake:late", "column": "done" })]
    );

    // With none in time, the macro aborts with the step unrun.
    f.state.borrow_mut().current = None;
    f.idle(0.1);
    f.key(Key::C);
    f.idle(MACRO_TIMEOUT_SECS + 0.2);
    assert_eq!(f.chrome.running_macro(), None, "aborted");
    assert_eq!(f.marked(), Vec::<serde_json::Value>::new());
}

/// A macro whose first step names `{current}` doesn't fire without one:
/// its key falls through, like a binding's.
#[tokio::test]
async fn a_first_step_naming_current_needs_one_to_fire() {
    let mut f = Fixture::new(KEYMAP);
    f.key(Key::X);
    assert_eq!(f.chrome.running_macro(), None);
    assert!(f.seen().contains(&Key::X), "the app gets the key");

    f.state.borrow_mut().current = Some("fake:one".to_owned());
    f.frame(Vec::new());
    f.key(Key::X);
    assert_eq!(f.marked(), [json!({ "card": "fake:one" })]);
}

/// A step's result reaches later steps: by its `as` name, and as `{prev}`
/// to the step right after it. A whole-string placeholder keeps the value's
/// type; one inside text is written in.
#[test]
fn a_step_hands_its_result_to_later_steps() {
    let mut f = Fixture::new(
        r#"
        [[bind]]
        scope = "fake"
        keys = "b"
        macro = "bound"

        [macro.bound]
        steps = [
          { tool = "fake_answer", as = "a" },
          { tool = "fake_mark", args = { from_prev = "x-{prev.ref}", n = "{a.n}", whole = "{prev}" } },
          { tool = "fake_mark", args = { still = "{a.ref}" } },
        ]
        "#,
    );
    f.key(Key::B);
    f.idle(0.2);
    assert_eq!(f.chrome.running_macro(), None, "done");
    assert_eq!(
        f.marked(),
        [
            serde_json::json!({ "from_prev": "x-fake:1", "n": 2, "whole": common::answer() }),
            serde_json::json!({ "still": "fake:1" }),
        ]
    );
}

/// A step bound with `as` that answers with nothing aborts the run before
/// the step that needed it.
#[test]
fn an_as_step_with_nothing_to_bind_aborts() {
    let mut f = Fixture::new(
        r#"
        [[bind]]
        scope = "fake"
        keys = "b"
        macro = "empty"

        [macro.empty]
        steps = [
          { tool = "fake_down", as = "a" },
          { tool = "fake_mark", args = { x = "{a}" } },
        ]
        "#,
    );
    f.key(Key::B);
    f.idle(0.2);
    assert_eq!(f.calls(), ["fake_down"]);
    assert_eq!(f.chrome.running_macro(), None, "aborted");
    assert!(f.marked().is_empty(), "the step after never ran");
}

/// The shipped `E` in Dave's normal mode, through the real Headway.
#[cfg(feature = "headway")]
mod session_explainer {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use egui::{Event, Key, Modifiers};
    use headway::event::{build_review, ReviewFields, KIND_ISSUE};
    use nostrdb::{Filter, Transaction};
    use nostrdb_net::{FullKeypair, NoteId};
    use notedeck::{
        App, AppAction, AppContext, AppResponse, Notedeck, OpenUri, ToolOutcome, ToolSpec,
    };
    use notedeck_chrome::testing::ChromeDriver;
    use notedeck_chrome::{Chrome, NotedeckApp};
    use notedeck_headway::{store, Headway};
    use serde_json::Value;

    /// The demo card given a review record with an explainer.
    const CARD: &str = "Inline card creation";

    /// A demo card with no review record.
    const BARE_CARD: &str = "Column reordering";

    /// The explainer [`CARD`]'s record names.
    const EXPLAINER: &str = "https://example.com/inline-card-creation";

    /// The grid's summary line once the demo board has folded.
    const GRID: &str = "7 cards · 5 columns";

    /// How many frames a wait gives up after: ten seconds of egui time,
    /// well past a macro step's three.
    const SETTLE_FRAMES: usize = 600;

    /// What the stand-in for Dave saw.
    #[derive(Default)]
    struct StandInState {
        /// Whether it rendered on the last frame.
        rendered: bool,
        /// Keys whose press reached its render.
        seen: Vec<Key>,
    }

    /// Dave in normal mode, as chrome sees it: the `dave.normal` scope, and
    /// `dave_open_issue` available while the session has an issue, answering
    /// with an open of it.
    struct DaveStandIn {
        issue: Option<String>,
        state: Rc<RefCell<StandInState>>,
    }

    impl App for DaveStandIn {
        fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
            let mut state = self.state.borrow_mut();
            state.rendered = true;
            let pressed: Vec<Key> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Key {
                            key, pressed: true, ..
                        } => Some(*key),
                        _ => None,
                    })
                    .collect()
            });
            state.seen.extend(pressed);
            ui.label("dave stand-in");
            AppResponse::none()
        }

        fn ui_tools(&self) -> Vec<ToolSpec> {
            vec![
                ToolSpec::new(
                    "dave_open_issue",
                    "Open the active session's issue.",
                    Vec::new(),
                ),
                ToolSpec::new(
                    "dave_session_issue",
                    "The active session's issue, as data.",
                    Vec::new(),
                ),
            ]
        }

        /// `dave_session_issue` answers with the issue as real Dave does
        /// (`notedeck_dave`'s `session_issue_answers_with_the_issue_and_changes_nothing`);
        /// `dave_open_issue` asks chrome to open it.
        fn call_ui_tool(
            &mut self,
            _ctx: &mut AppContext<'_>,
            name: &str,
            _args: &Value,
        ) -> ToolOutcome {
            let Some(issue) = &self.issue else {
                return ToolOutcome::Error("the session has no issue".to_owned());
            };
            if name == "dave_session_issue" {
                return ToolOutcome::Data(serde_json::json!({ "issue": issue }));
            }
            ToolOutcome::Intent(AppAction::Open(OpenUri {
                reference: issue.clone(),
                msg: None,
            }))
        }

        fn ui_tool_available(&self, name: &str) -> bool {
            matches!(name, "dave_open_issue" | "dave_session_issue") && self.issue.is_some()
        }

        fn key_scopes(&self) -> &'static [&'static str] {
            &["dave.normal"]
        }
    }

    /// A headless chrome with the stand-in Dave in front and the real
    /// Headway behind it, on the demo board, which Headway has folded.
    struct Session {
        _dir: tempfile::TempDir,
        driver: ChromeDriver,
        dave: Rc<RefCell<StandInState>>,
    }

    /// The demo card titled `title`.
    fn card_id(ndb: &nostrdb::Ndb, title: &str) -> NoteId {
        let txn = Transaction::new(ndb).expect("txn");
        let filter = Filter::new().kinds([KIND_ISSUE.into()]).build();
        let results = ndb.query(&txn, &[filter], 100).expect("query");
        results
            .iter()
            .find(|r| {
                r.note.tags().iter().any(|tag| {
                    tag.count() >= 2
                        && tag.get_str(0) == Some("subject")
                        && tag.get_str(1) == Some(title)
                })
            })
            .map(|r| NoteId::new(*r.note.id()))
            .unwrap_or_else(|| panic!("no demo card {title:?}"))
    }

    impl Session {
        /// Seed the demo board, give [`CARD`] a review record with an
        /// explainer, and start the chrome with a Dave stand-in whose session
        /// works `issue` (a demo card's title), or none. Navigation doesn't
        /// slide.
        async fn new(issue: Option<&str>) -> Self {
            Self::start(issue, false).await
        }

        /// [`Session::new`], but navigation slides as it does for a user
        /// with the default settings, so a macro step can land mid-slide.
        async fn animated(issue: Option<&str>) -> Self {
            Self::start(issue, true).await
        }

        /// [`Session::new`], with nav transitions animated when `animate`.
        async fn start(issue: Option<&str>, animate: bool) -> Self {
            let dir = tempfile::TempDir::new().expect("tmp dir");
            let kp = FullKeypair::generate();
            let args: Vec<String> = [
                "notedeck-test",
                "--testrunner",
                "--nsec",
                &kp.secret_key.to_secret_hex(),
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let egui_ctx = egui::Context::default();
            let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
            let issue_ref = {
                let app_ctx = notedeck.app_context();
                app_ctx.settings.complete_welcome();
                app_ctx.settings.get_settings_mut().animate_nav_transitions = animate;
                let ndb = &*app_ctx.ndb;
                let secret = kp.secret_key.secret_bytes();
                let mine = || Filter::new().authors([kp.pubkey.bytes()]).build();

                let sub = ndb.subscribe(&[mine()]).expect("subscribe");
                let written = store::seed_demo_board(
                    ndb,
                    &kp.pubkey,
                    &secret,
                    store::BOARD_ID,
                    1_700_000_000,
                    &mut store::NoPublish,
                );
                let written = u32::try_from(written).expect("a demo board's worth");
                ndb.wait_for_all_notes_within(sub, written, Duration::from_secs(10))
                    .await
                    .expect("the demo board seeds");

                let card = card_id(ndb, CARD);
                let review = ReviewFields {
                    explainer: Some(EXPLAINER.to_owned()),
                    ..Default::default()
                };
                let sub = ndb.subscribe(&[mine()]).expect("subscribe");
                store::ingest(
                    ndb,
                    build_review(&card, &review).created_at(1_700_000_100),
                    &secret,
                    &mut store::NoPublish,
                )
                .expect("the review record ingests");
                ndb.wait_for_all_notes_within(sub, 1, Duration::from_secs(10))
                    .await
                    .expect("the review record lands");

                issue.map(|title| {
                    headway::wordid::card_ref(store::BOARD_ID, card_id(ndb, title).bytes())
                })
            };
            let dave = Rc::new(RefCell::new(StandInState::default()));
            let stand_in = DaveStandIn {
                issue: issue_ref,
                state: Rc::clone(&dave),
            };
            let chrome = Chrome::new_headless_with_apps(
                &mut notedeck,
                [
                    NotedeckApp::Other("dave".to_owned(), Box::new(stand_in)),
                    NotedeckApp::Headway(Box::new(Headway::new())),
                ],
            );

            let mut session = Self {
                _dir: dir,
                driver: ChromeDriver::new(egui_ctx, notedeck, chrome),
                dave,
            };
            // Open Headway once so its board has folded, then go back to
            // Dave, where the key is pressed.
            session.driver.chrome.set_active(1);
            session.wait_for("the board to fold", |s| s.drew(GRID));
            session.driver.chrome.set_active(0);
            session.wait_for("Dave in front", |s| s.dave.borrow().rendered);
            session
        }

        /// One real chrome frame ([`ChromeDriver::frame`]), noting whether
        /// the stand-in Dave rendered in it.
        fn frame(&mut self, events: Vec<Event>) {
            self.dave.borrow_mut().rendered = false;
            self.driver.frame(events);
        }

        /// Run frames until `done`, or panic naming `what`.
        fn wait_for(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
            for _ in 0..SETTLE_FRAMES {
                self.frame(Vec::new());
                if done(self) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!(
                "timed out waiting for {what}; last frame drew {:?}",
                self.driver.drawn()
            );
        }

        /// Whether the last frame painted exactly `text`.
        fn drew(&self, text: &str) -> bool {
            self.driver.drew(text)
        }

        /// The id of the column the card `card` (a ref) is in, as Headway's
        /// `headway_show_board` data tool reads the board.
        fn column_of(&mut self, card: &str) -> Option<String> {
            let ToolOutcome::Data(board) = self
                .driver
                .notedeck
                .app_context()
                .call_tool("headway_show_board", &serde_json::json!({}))
            else {
                panic!("the demo board shows");
            };
            board["columns"]
                .as_array()?
                .iter()
                .find(|column| {
                    column["cards"]
                        .as_array()
                        .is_some_and(|cards| cards.iter().any(|c| c["ref"] == card))
                })
                .and_then(|column| column["id"].as_str().map(str::to_owned))
        }

        /// Press Shift+E.
        fn press_e(&mut self) {
            let event = |pressed| Event::Key {
                key: Key::E,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            };
            self.frame(vec![event(true), Event::Text("E".to_owned()), event(false)]);
        }
    }

    /// `E` in Dave's normal mode opens the session's card in Headway and
    /// then its explainer: one key for what was `s` then `e`.
    #[tokio::test]
    async fn e_opens_the_sessions_card_and_its_explainer() {
        let mut s = Session::new(Some(CARD)).await;
        s.press_e();
        assert_eq!(s.driver.chrome.running_macro(), Some("session-explainer"));
        s.wait_for("the macro to finish", |s| {
            s.driver.chrome.running_macro().is_none()
        });
        // Headway runs a tool call on its next render.
        s.frame(Vec::new());

        assert_eq!(
            s.driver.opened(),
            [EXPLAINER],
            "the card's explainer opened"
        );
        assert!(!s.dave.borrow().rendered, "Headway is in front");
        assert!(s.drew(CARD), "on the card");
        assert!(!s.drew(GRID), "its detail, not the grid");
        assert!(!s.dave.borrow().seen.contains(&Key::E), "E was the macro's");
    }

    /// A card with no review record has no explainer: the macro lands on the
    /// card, Headway says there's none, and nothing opens.
    #[tokio::test]
    async fn e_on_a_card_with_no_record_opens_nothing() {
        let mut s = Session::new(Some(BARE_CARD)).await;
        s.press_e();
        s.wait_for("the macro to finish", |s| {
            s.driver.chrome.running_macro().is_none()
        });
        s.frame(Vec::new());
        assert!(
            s.driver.opened().is_empty(),
            "nothing to open: {:?}",
            s.driver.opened()
        );
        assert!(s.drew(BARE_CARD), "on the card");
    }

    /// jb55's "card done" macro: the session's issue, bound with `as`, is
    /// the card `headway_move_card` moves to Done. Nothing navigates; Dave
    /// stays in front.
    #[tokio::test]
    async fn a_bound_issue_moves_the_sessions_card_to_done() {
        let mut s = Session::new(Some(CARD)).await;
        let user = r#"
            [[bind]]
            scope = "dave.normal"
            keys = "D"
            macro = "session-done"

            [macro.session-done]
            label = "card done"
            steps = [
              { tool = "dave_session_issue", as = "card" },
              { tool = "headway_move_card", args = { card = "{card.issue}", column = "done" } },
            ]
        "#;
        let keymap = notedeck_chrome::keymap::parse(&format!(
            "{}\n{user}",
            notedeck_chrome::keymap::DEFAULT_KEYMAP
        ))
        .expect("the keymap parses");
        s.driver.chrome.set_keymap(keymap);
        let card = {
            let ctx = s.driver.notedeck.app_context();
            headway::wordid::card_ref(store::BOARD_ID, card_id(ctx.ndb, CARD).bytes())
        };
        assert_ne!(s.column_of(&card).as_deref(), Some("done"));

        let event = |pressed| Event::Key {
            key: Key::D,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        };
        s.frame(vec![event(true), Event::Text("D".to_owned()), event(false)]);
        s.wait_for("the macro to finish", |s| {
            s.driver.chrome.running_macro().is_none()
        });
        // The move is a note Headway folds in.
        for _ in 0..SETTLE_FRAMES {
            if s.column_of(&card).as_deref() == Some("done") {
                break;
            }
            s.frame(Vec::new());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            s.column_of(&card).as_deref(),
            Some("done"),
            "the card is in Done"
        );
        assert!(s.dave.borrow().rendered, "Dave stays in front");
        assert!(!s.dave.borrow().seen.contains(&Key::D), "D was the macro's");
    }

    /// A demo card that's a subissue of the demo board's epic, "Define nostr
    /// event model for boards".
    const SUBISSUE: &str = "Sync cards across relays";

    /// The epic's other subissue, which [`epic_review_session`] moves to In
    /// Review.
    const EPIC_DONE: &str = "Scaffold the Headway app crate";

    /// jb55's "epic review queue" macro on `R`: the session's card, its
    /// parent, then the review queue over the parent's In Review subissues.
    const EPIC_REVIEW: &str = r#"
        [[bind]]
        scope = "dave.normal"
        keys = "R"
        macro = "session-epic-review"

        [macro.session-epic-review]
        label = "epic review queue"
        steps = [
          { tool = "dave_open_issue" },
          { tool = "headway_open_parent" },
          { tool = "headway_open_review_queue" },
        ]
    "#;

    impl Session {
        /// Merge `user` over the shipped keymap, as an account's overrides.
        fn set_user_keymap(&mut self, user: &str) {
            let keymap = notedeck_chrome::keymap::parse(&format!(
                "{}\n{user}",
                notedeck_chrome::keymap::DEFAULT_KEYMAP
            ))
            .expect("the keymap parses");
            self.driver.chrome.set_keymap(keymap);
        }

        /// Press Shift+`key`, typing `text`.
        fn press_shift(&mut self, key: Key, text: &str) {
            let event = |pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            };
            self.frame(vec![
                event(true),
                Event::Text(text.to_owned()),
                event(false),
            ]);
        }
    }

    /// A session on `issue` with [`EPIC_REVIEW`] bound and [`EPIC_DONE`]
    /// moved to In Review, folded. Navigation slides: `headway_open_parent`
    /// pushes one card's detail over another's, and the next step must wait
    /// for that slide to land, not just for the push to apply.
    async fn epic_review_session(issue: &str) -> Session {
        let mut s = Session::animated(Some(issue)).await;
        s.set_user_keymap(EPIC_REVIEW);
        let done = {
            let ctx = s.driver.notedeck.app_context();
            headway::wordid::card_ref(store::BOARD_ID, card_id(ctx.ndb, EPIC_DONE).bytes())
        };
        let moved = s.driver.notedeck.app_context().call_tool(
            "headway_move_card",
            &serde_json::json!({ "card": done, "column": "in-review" }),
        );
        assert!(matches!(moved, ToolOutcome::Data(_)), "the move applies");
        for _ in 0..SETTLE_FRAMES {
            if s.column_of(&done).as_deref() == Some("in-review") {
                return s;
            }
            s.frame(Vec::new());
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{EPIC_DONE} never landed in In Review");
    }

    /// The macro from a session on a subissue opens its parent, the epic,
    /// and the review queue over the epic's In Review subissues: one key
    /// from a session to its epic's review.
    #[tokio::test]
    async fn the_epic_review_macro_opens_the_parents_queue() {
        let mut s = epic_review_session(SUBISSUE).await;
        s.press_shift(Key::R, "R");
        assert_eq!(s.driver.chrome.running_macro(), Some("session-epic-review"));
        s.wait_for("the macro to finish", |s| {
            s.driver.chrome.running_macro().is_none()
        });
        s.wait_for("the epic's review queue", |s| s.drew("1 / 1"));
        assert!(s.drew(EPIC_DONE), "the queue shows the epic's subissue");
        assert!(!s.dave.borrow().seen.contains(&Key::R), "R was the macro's");
    }

    /// On a card with no parent `headway_open_parent` is never offered, so
    /// the macro waits out its step's timeout and aborts on the session's
    /// card, opening no queue.
    #[tokio::test]
    async fn the_epic_review_macro_stops_on_a_card_with_no_parent() {
        let mut s = epic_review_session(CARD).await;
        let pressed = s.driver.time();
        s.press_shift(Key::R, "R");
        assert_eq!(s.driver.chrome.running_macro(), Some("session-epic-review"));
        s.wait_for("the macro to abort", |s| {
            s.driver.chrome.running_macro().is_none()
        });
        assert!(
            s.driver.time() - pressed >= notedeck_chrome::keymap::macros::MACRO_TIMEOUT_SECS,
            "the run waited out its step, not finished"
        );
        s.frame(Vec::new());
        assert!(s.drew(CARD), "on the session's card");
        assert!(!s.drew("1 / 1"), "no queue opened");
    }

    /// A session with no issue has no `dave_open_issue`, so `E` isn't the
    /// macro's: it falls through to Dave.
    #[tokio::test]
    async fn e_without_an_issue_falls_through_to_dave() {
        let mut s = Session::new(None).await;
        s.press_e();
        assert_eq!(s.driver.chrome.running_macro(), None);
        assert!(s.dave.borrow().seen.contains(&Key::E), "Dave gets the key");
        s.frame(Vec::new());
        assert!(s.dave.borrow().rendered, "still in Dave");
        assert!(s.driver.opened().is_empty());
    }
}

/// The which-key strip lists a macro binding by its label.
#[test]
fn the_strip_lists_a_macro_by_its_label() {
    let mut f = Fixture::new(KEYMAP);
    f.press(Modifiers::SHIFT, Key::Questionmark, Some("?"));
    f.frame(Vec::new());
    assert!(f.drew("down then first"), "drew {:?}", f.drawn());
}
