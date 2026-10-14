//! Headway's keys through the real chrome: Headway reads no keys of its own,
//! so every one goes chrome's keymap → the shipped `default_keymap.toml` →
//! Headway's UI tool, or, for a board edit, Headway's data tool on its
//! current card. A headless chrome runs the real Headway, on a seeded demo
//! board, behind a wrapper that records each UI tool call reaching it; keys
//! go in as real chrome frames.
//!
//! What each key then does to the board is Headway's own tests' business
//! (`notedeck_headway`'s `keys.rs` tests run the same engine and keymap); these
//! check the wiring: Headway's key scopes reach the engine, availability goes
//! through the registry to Headway, the call lands on Headway, and the keymap
//! names only tools Headway owns.

#![cfg(feature = "headway")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use egui::{Event, Key, Modifiers};
use nostrdb::Filter;
use nostrdb_net::FullKeypair;
use notedeck::{App, AppContext, AppResponse, Notedeck, ToolInvocation, ToolOutcome, ToolSpec};
use notedeck_chrome::keymap::DEFAULT_KEYMAP;
use notedeck_chrome::{Chrome, NotedeckApp};
use notedeck_headway::{store, Headway};
use serde_json::{json, Value};

/// The grid's summary line once the demo board has folded.
const GRID: &str = "7 cards · 5 columns";

/// How many frames a wait for the board gives up after.
const SETTLE_FRAMES: usize = 600;

/// Headway, recording every UI tool call chrome hands it before passing it on.
struct Recording {
    headway: Headway,
    calls: Rc<RefCell<Vec<ToolInvocation>>>,
}

impl App for Recording {
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        self.headway.update(ctx);
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.headway.render(ctx, ui)
    }

    fn render_nav(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        token: &Rc<dyn std::any::Any>,
    ) -> AppResponse {
        self.headway.render_nav(ctx, ui, token)
    }

    fn tools(&self) -> Vec<notedeck::RegisteredTool> {
        self.headway.tools()
    }

    fn ui_tools(&self) -> Vec<ToolSpec> {
        self.headway.ui_tools()
    }

    fn call_ui_tool(&mut self, ctx: &mut AppContext<'_>, name: &str, args: &Value) -> ToolOutcome {
        self.calls
            .borrow_mut()
            .push(ToolInvocation::new(name, args.clone()));
        self.headway.call_ui_tool(ctx, name, args)
    }

    fn ui_tool_available(&self, name: &str) -> bool {
        self.headway.ui_tool_available(name)
    }

    fn key_scopes(&self) -> &'static [&'static str] {
        self.headway.key_scopes()
    }

    fn current_ref(&self) -> Option<&str> {
        self.headway.current_ref()
    }

    fn search(
        &mut self,
        ctx: &mut AppContext<'_>,
        query: &notedeck::SearchQuery<'_>,
        out: &mut notedeck::SearchHits,
    ) {
        self.headway.search(ctx, query, out);
    }
}

/// A headless chrome with the recording Headway in front, on the demo board.
struct Fixture {
    _dir: tempfile::TempDir,
    egui_ctx: egui::Context,
    notedeck: Notedeck,
    chrome: Chrome,
    calls: Rc<RefCell<Vec<ToolInvocation>>>,
    /// The input time of the next frame.
    time: f64,
    /// Every piece of text the last frame painted.
    drawn: Vec<String>,
}

impl Fixture {
    /// Seed the demo board as a fresh account, start the chrome with Headway
    /// alone (the nav root, so nothing slides) and wait for the grid.
    async fn new() -> Self {
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
        {
            let app_ctx = notedeck.app_context();
            app_ctx.settings.complete_welcome();
            app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
            let ndb = &*app_ctx.ndb;
            let sub = ndb
                .subscribe(&[Filter::new().authors([kp.pubkey.bytes()]).build()])
                .expect("subscribe");
            let written = store::seed_demo_board(
                ndb,
                &kp.pubkey,
                &kp.secret_key.secret_bytes(),
                store::BOARD_ID,
                1_700_000_000,
                &mut store::NoPublish,
            );
            let written = u32::try_from(written).expect("a demo board's worth of notes");
            ndb.wait_for_all_notes_within(sub, written, Duration::from_secs(10))
                .await
                .expect("the demo board seeds");
        }
        notedeck.setup(&egui_ctx);

        let calls = Rc::new(RefCell::new(Vec::new()));
        let headway = Recording {
            headway: Headway::new(),
            calls: Rc::clone(&calls),
        };
        let mut chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [NotedeckApp::Other("headway".to_owned(), Box::new(headway))],
        );
        let update = App::take_tool_update(&mut chrome).expect("the running set changed");
        notedeck.apply_tool_update(update);

        let mut fixture = Self {
            _dir: dir,
            egui_ctx,
            notedeck,
            chrome,
            calls,
            time: 0.0,
            drawn: Vec::new(),
        };
        fixture.wait_to_draw(GRID);
        fixture
    }

    /// One real chrome frame, `update` then `render`, with `events` as input.
    fn frame(&mut self, events: Vec<Event>) {
        let input = egui::RawInput {
            time: Some(self.time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        let Self {
            egui_ctx,
            notedeck,
            chrome,
            drawn,
            ..
        } = self;
        let mut app_ctx = notedeck.app_context();
        let output = egui_ctx.run_ui(input, |ui| {
            chrome.update(&mut app_ctx);
            egui::CentralPanel::default().show(ui, |ui| {
                chrome.render(&mut app_ctx, ui);
            });
        });
        drawn.clear();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, drawn);
        }
        output.drop_without_applying_deltas();
    }

    /// Run frames until `text` is painted, or panic.
    fn wait_to_draw(&mut self, text: &str) {
        for _ in 0..SETTLE_FRAMES {
            self.frame(Vec::new());
            if self.drew(text) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{text:?} never drawn; last frame drew {:?}", self.drawn);
    }

    /// Whether the last frame painted exactly `text`.
    fn drew(&self, text: &str) -> bool {
        self.drawn.iter().any(|drawn| drawn == text)
    }

    /// One frame with `key` pressed (typing `text`, if any) and released, then
    /// one to let it land.
    fn press(&mut self, modifiers: Modifiers, key: Key, text: Option<&str>) {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let mut events = vec![event(true)];
        if let Some(text) = text {
            events.push(Event::Text(text.to_owned()));
        }
        events.push(event(false));
        self.frame(events);
        self.frame(Vec::new());
    }

    /// Press a bare key.
    fn key(&mut self, key: Key) {
        self.press(Modifiers::NONE, key, None);
    }

    /// The demo board as Headway's `headway_show_board` data tool reads it.
    fn board(&mut self) -> Value {
        match self
            .notedeck
            .app_context()
            .call_tool("headway_show_board", &json!({}))
        {
            ToolOutcome::Data(board) => board,
            _ => panic!("the demo board shows"),
        }
    }

    /// Run frames until the card `card` is in column `col` of the board, or
    /// off it for `None`, or panic.
    fn wait_for_card_in(&mut self, card: &str, col: Option<usize>) {
        for _ in 0..SETTLE_FRAMES {
            let board = self.board();
            let at = board["columns"]
                .as_array()
                .expect("columns")
                .iter()
                .position(|column| {
                    let cards = column["cards"].as_array().expect("cards");
                    cards.iter().any(|c| c["ref"] == card)
                });
            if at == col {
                return;
            }
            self.frame(Vec::new());
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{card} never landed in column {col:?}");
    }

    /// Take the UI tool calls that reached Headway since the last take.
    fn take_calls(&self) -> Vec<ToolInvocation> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }
}

/// Push the text of every text shape in `shape` onto `out`.
fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, out);
            }
        }
        _ => {}
    }
}

/// A call of `name` with `args`.
fn call(name: &str, args: Value) -> ToolInvocation {
    ToolInvocation::new(name, args)
}

/// One `[[bind]]` of the shipped keymap, as written.
#[derive(Debug)]
struct Binding {
    scope: String,
    tool: String,
    /// Its args as JSON, `Value::Null` when it has none.
    args: Value,
}

/// The shipped keymap as written: every `[[bind]]` that names a tool.
fn default_bindings() -> Vec<Binding> {
    let table: toml::Table = DEFAULT_KEYMAP.parse().expect("the default keymap is TOML");
    let binds = table["bind"].as_array().expect("[[bind]] entries");
    binds
        .iter()
        .filter_map(|bind| {
            let scope = bind["scope"].as_str()?.to_owned();
            let tool = bind.get("tool")?.as_str()?.to_owned();
            let args = bind
                .get("args")
                .map(|args| serde_json::to_value(args).expect("args as JSON"))
                .unwrap_or(Value::Null);
            Some(Binding { scope, tool, args })
        })
        .collect()
}

/// Every Headway binding in the shipped keymap names a UI tool Headway owns
/// in the host's registry (not a name a data tool shadows), and Headway takes
/// its args; or one of Headway's data tools, on the current card.
#[tokio::test]
async fn every_headway_binding_names_a_headway_tool() {
    let mut f = Fixture::new().await;
    let headway = notedeck::AppId(0);
    let data_tools: Vec<_> = Headway::new()
        .tools()
        .iter()
        .map(|tool| tool.name())
        .collect();
    let bindings: Vec<_> = default_bindings()
        .into_iter()
        .filter(|binding| binding.scope.starts_with("headway"))
        .collect();
    assert!(bindings.len() > 30, "Headway's keymap is in: {bindings:?}");

    for Binding { scope, tool, args } in bindings {
        if data_tools.contains(&tool.as_str()) {
            assert_eq!(
                args["card"], "{current}",
                "{scope} binds the data tool {tool} to the current card"
            );
            continue;
        }
        let owner = f.notedeck.app_context().registries.tools.ui_owner(&tool);
        assert!(
            matches!(owner, Some(notedeck::ToolOwner::App(id)) if id == headway),
            "{scope} binds {tool}, which isn't Headway's tool ({owner:?})"
        );
        let invocation = ToolInvocation::new(tool.clone(), args.clone());
        let outcome = f
            .chrome
            .call_ui_tool(&mut f.notedeck.app_context(), &invocation);
        if let ToolOutcome::Error(err) = outcome {
            panic!("{scope} binds {tool} {args}: {err}");
        }
    }
}

/// On the grid, keys reach Headway's tools through chrome, `g g` as a
/// chord; `?` pins chrome's strip of Headway's keys; Esc drops the cursor,
/// then the strip, and then, with nothing left, isn't Headway's.
#[tokio::test]
async fn grid_keys_reach_headway_through_chrome() {
    let mut f = Fixture::new().await;

    f.key(Key::J);
    f.key(Key::L);
    f.key(Key::G);
    assert_eq!(
        f.take_calls(),
        [
            call("headway_cursor_move", json!({ "to": "down" })),
            call("headway_cursor_move", json!({ "to": "right" })),
        ],
        "g waits for its second key"
    );
    f.key(Key::G);
    assert_eq!(
        f.take_calls(),
        [call("headway_cursor_move", json!({ "to": "first" }))]
    );

    f.press(Modifiers::SHIFT, Key::Questionmark, Some("?"));
    assert!(f.chrome.key_hints_pinned());
    for text in ["up/down", "columns", "first/last", "open", "explainer"] {
        assert!(f.drew(text), "{text:?} not in the strip: {:?}", f.drawn);
    }
    assert!(f.take_calls().is_empty(), "? is chrome's");

    f.key(Key::Escape);
    assert_eq!(f.take_calls(), [call("headway_clear", json!({}))]);
    assert!(f.chrome.key_hints_pinned(), "the strip is chrome's Esc");
    f.key(Key::Escape);
    assert!(!f.chrome.key_hints_pinned());
    f.key(Key::Escape);
    assert!(
        f.take_calls().is_empty(),
        "nothing to clear: Esc isn't Headway's"
    );
}

/// The board edits (`a`, `D`, `H L J K`) are Headway's data tools on its
/// current card, run by chrome on the real board: no UI tool runs, the edit
/// lands in nostrdb, and the cursor follows the card as each folds in, so
/// the next key edits the same card.
#[tokio::test]
async fn board_edit_keys_edit_the_current_card() {
    let mut f = Fixture::new().await;
    // Off the grid: no current card, so `a` is unbound and does nothing.
    f.key(Key::A);
    f.wait_to_draw(GRID);

    f.key(Key::J);
    assert_eq!(
        f.take_calls(),
        [call("headway_cursor_move", json!({ "to": "down" }))]
    );
    let card = f.board()["columns"][0]["cards"][0]["ref"]
        .as_str()
        .expect("the first card's ref")
        .to_owned();

    f.press(Modifiers::SHIFT, Key::L, None);
    f.wait_for_card_in(&card, Some(1));
    f.press(Modifiers::SHIFT, Key::D, None);
    let done = f.board()["columns"].as_array().expect("columns").len() - 1;
    f.wait_for_card_in(&card, Some(done));
    f.key(Key::A);
    f.wait_for_card_in(&card, None);
    f.wait_to_draw("6 cards · 5 columns");
    assert!(f.take_calls().is_empty(), "board edits call no UI tool");
}

/// A focused text field keeps its keys: `/` focuses the grid's filter, and
/// what's typed then goes into it, not to the keymap.
#[tokio::test]
async fn a_focused_field_keeps_its_keys() {
    let mut f = Fixture::new().await;
    f.press(Modifiers::NONE, Key::Slash, Some("/"));
    assert_eq!(f.take_calls(), [call("headway_focus_filter", json!({}))]);

    f.press(Modifiers::NONE, Key::J, Some("j"));
    f.press(Modifiers::NONE, Key::K, Some("k"));
    assert!(f.take_calls().is_empty(), "the filter took j and k");
    f.wait_to_draw("jk");
}

/// Press the bar's key, Cmd-K on macOS and Ctrl-Space elsewhere
/// ([`notedeck_chrome::open_shortcut`]), the way this platform's keyboard
/// reports it: off macOS, Ctrl sets `command` too.
fn cmd_k(f: &mut Fixture) {
    let open = notedeck_chrome::open_shortcut(f.egui_ctx.os());
    let modifiers = match open.modifiers.ctrl {
        true => open.modifiers | Modifiers::COMMAND,
        false => open.modifiers,
    };
    f.press(modifiers, open.logical_key, None);
}

/// The command bar's selected row's title.
#[cfg(not(debug_assertions))]
fn selected(f: &mut Fixture) -> Option<String> {
    f.chrome
        .command_bar_mut()
        .selected()
        .map(|item| item.title.clone())
}

/// Headway answers the bar's search (headway:notedeck/tuition-raw-benefit):
/// a word from a demo card's title lists that card under Headway, marked as a
/// card on its board and column, and a keyed term narrows it the way the
/// issue list's query does. Opening a hit is Headway's own test's business
/// (`search_finds_boards_and_cards_and_opens_a_card_on_its_board`): this
/// fixture runs Headway as an `Other` app, which the chrome's open routing
/// doesn't send Headway's notes to.
#[tokio::test]
async fn the_command_bar_searches_headway_cards() {
    const CARD: &str = "Sync cards across relays";
    let mut f = Fixture::new().await;
    cmd_k(&mut f);
    f.frame(vec![Event::Text("relays".to_owned())]);
    f.frame(Vec::new());

    let listed = |f: &mut Fixture| -> Vec<String> {
        f.chrome
            .command_bar_mut()
            .matches()
            .map(|item| item.title.clone())
            .collect()
    };
    assert!(
        listed(&mut f).iter().any(|t| t == CARD),
        "{:?}",
        listed(&mut f)
    );
    assert!(
        f.drawn
            .iter()
            .any(|d| d.contains(CARD) && d.contains("Card")),
        "drawn as a card: {:?}",
        f.drawn
    );
    assert!(
        f.drawn.iter().any(|d| d.contains("Headway · Backlog")),
        "on its board and column: {:?}",
        f.drawn
    );

    // `label:` is the issue list's term, and this card is labelled `nostr`.
    f.frame(vec![Event::Text(" label:ui".to_owned())]);
    f.frame(Vec::new());
    assert!(
        !listed(&mut f).iter().any(|t| t == CARD),
        "{:?}",
        listed(&mut f)
    );
}

/// Cmd-K lists Headway's tools; "issue list" picks Open issue list, and
/// Enter runs it on Headway, which opens the list.
///
/// Release only, with its own CI step, as is every test that opens the
/// list: its push slides through egui_nav, which draws both routes on two
/// layers with repeating widget ids, and egui debug-asserts against that
/// (see `chrome/headway_nav_tests.rs`).
#[cfg(not(debug_assertions))]
#[tokio::test]
async fn the_command_bar_opens_the_issue_list() {
    let mut f = Fixture::new().await;
    cmd_k(&mut f);
    assert!(f.chrome.command_bar().is_open());
    f.frame(vec![Event::Text("issue list".to_owned())]);
    assert_eq!(selected(&mut f).as_deref(), Some("Open issue list"));

    f.key(Key::Enter);
    assert!(!f.chrome.command_bar().is_open());
    let calls = f.take_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, "headway_open_list");
    f.wait_to_draw("All issues");
}

/// An enum argument from the command bar: Group list by asks what to group
/// by, listing Headway's own values, and the one picked reaches Headway.
/// Release only, as the test above.
#[cfg(not(debug_assertions))]
#[tokio::test]
async fn the_command_bar_groups_the_list_by_a_picked_value() {
    let mut f = Fixture::new().await;
    cmd_k(&mut f);
    f.frame(vec![Event::Text("issue list".to_owned())]);
    f.key(Key::Enter);
    f.wait_to_draw("All issues");
    f.take_calls();

    cmd_k(&mut f);
    f.frame(vec![Event::Text("group list".to_owned())]);
    assert_eq!(selected(&mut f).as_deref(), Some("Group list by"));
    f.key(Key::Enter);
    assert_eq!(f.chrome.command_bar().prompt(), Some("Group list by"));
    let values: Vec<String> = f
        .chrome
        .command_bar_mut()
        .matches()
        .map(|item| item.title.clone())
        .collect();
    for value in ["Next", "Status", "Priority", "None"] {
        assert!(values.iter().any(|v| v == value), "{value}: {values:?}");
    }
    assert!(f.take_calls().is_empty(), "nothing ran yet");

    f.frame(vec![Event::Text("priority".to_owned())]);
    f.key(Key::Enter);
    assert!(!f.chrome.command_bar().is_open());
    assert_eq!(
        f.take_calls(),
        [call("headway_list_group", json!({ "by": "priority" }))]
    );
}
