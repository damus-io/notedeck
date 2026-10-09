//! The chrome's keymap engine through real chrome frames: a fake app reports
//! the `fake` key scope and advertises UI tools, a test keymap binds keys to
//! them, and each test presses keys and checks which tools ran and which keys
//! still reached the app.

use std::cell::RefCell;
use std::rc::Rc;

use egui::{Event, Key, Modifiers};
use notedeck::{App, AppContext, AppResponse, Notedeck, ToolOutcome, ToolSpec};
use notedeck_chrome::keymap::{self, KeySeq};
use notedeck_chrome::{Chrome, NotedeckApp};
use notedeck_ui::chord::CHORD_TIMEOUT;

/// The test keymap: one binding per behaviour under test.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "j"
tool = "fake_down"

[[bind]]
scope = "fake"
keys = "g g"
tool = "fake_first"

[[bind]]
scope = "fake"
keys = "<C-f>"
tool = "fake_page"

[[bind]]
scope = "fake"
keys = "x"
tool = "fake_gated"

[[bind]]
scope = "elsewhere"
keys = "k"
tool = "fake_down"
"#;

/// The UI tools [`FakeApp`] advertises.
const TOOLS: [&str; 4] = ["fake_down", "fake_first", "fake_page", "fake_gated"];

/// The id of the text field [`FakeApp`] draws when asked to.
fn field_id() -> egui::Id {
    egui::Id::unique("keymap_engine_field")
}

/// What the fake app saw and did, shared with the test.
struct FakeState {
    /// UI tools that reached the app, in order.
    calls: Vec<String>,
    /// Keys whose press reached the app's render.
    seen: Vec<Key>,
    /// Whether `fake_gated` is available.
    gated_available: bool,
    /// Whether to draw a text field.
    show_field: bool,
    /// The text field's contents.
    text: String,
}

/// An app in the `fake` scope with [`TOOLS`] as UI tools.
struct FakeApp {
    state: Rc<RefCell<FakeState>>,
}

impl App for FakeApp {
    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut state = self.state.borrow_mut();
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
        if state.show_field {
            ui.add(egui::TextEdit::singleline(&mut state.text).id(field_id()));
        }
        AppResponse::none()
    }

    fn ui_tools(&self) -> Vec<ToolSpec> {
        TOOLS
            .iter()
            .map(|name| ToolSpec::new(name, "A fake UI tool.", Vec::new()))
            .collect()
    }

    fn call_ui_tool(
        &mut self,
        _ctx: &mut AppContext<'_>,
        name: &str,
        _args: &serde_json::Value,
    ) -> ToolOutcome {
        self.state.borrow_mut().calls.push(name.to_owned());
        ToolOutcome::Data(serde_json::Value::Null)
    }

    fn ui_tool_available(&self, name: &str) -> bool {
        name != "fake_gated" || self.state.borrow().gated_available
    }

    fn key_scopes(&self) -> &'static [&'static str] {
        &["fake"]
    }
}

/// A headless chrome with [`FakeApp`] in front and [`KEYMAP`] loaded.
struct Fixture {
    _dir: tempfile::TempDir,
    egui_ctx: egui::Context,
    notedeck: Notedeck,
    chrome: Chrome,
    state: Rc<RefCell<FakeState>>,
    time: f64,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let egui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
        {
            let app_ctx = notedeck.app_context();
            app_ctx.settings.complete_welcome();
            app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
        }
        notedeck.setup(&egui_ctx);
        let state = Rc::new(RefCell::new(FakeState {
            calls: Vec::new(),
            seen: Vec::new(),
            gated_available: true,
            show_field: false,
            text: String::new(),
        }));
        // The fake app alone, as the nav root: switching to it from another
        // app would slide, and a full chrome frame mid-slide trips egui's
        // debug assert on widget ids changing layer.
        let fake = FakeApp {
            state: Rc::clone(&state),
        };
        let mut chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [NotedeckApp::Other("fake".to_owned(), Box::new(fake))],
        );
        let update = App::take_tool_update(&mut chrome).expect("the running set changed");
        notedeck.apply_tool_update(update);
        chrome.set_keymap(keymap::parse(KEYMAP).expect("test keymap"));

        let mut fixture = Self {
            _dir: dir,
            egui_ctx,
            notedeck,
            chrome,
            state,
            time: 0.0,
        };
        fixture.frame(Vec::new());
        fixture.frame(Vec::new());
        fixture
    }

    /// One real chrome frame — `update` then `render`, as the host runs them
    /// — with `events` as its input, a sixtieth of a second after the last.
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
            ..
        } = self;
        let mut app_ctx = notedeck.app_context();
        egui_ctx
            .run_ui(input, |ui| {
                chrome.update(&mut app_ctx);
                egui::CentralPanel::default().show(ui, |ui| {
                    chrome.render(&mut app_ctx, ui);
                });
            })
            .drop_without_applying_deltas();
    }

    /// One frame with `key` pressed (typing `text`, if any) and released.
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
    }

    /// Press a bare key.
    fn key(&mut self, key: Key) {
        self.press(Modifiers::NONE, key, None);
    }

    /// Run idle frames until `seconds` of input time have passed.
    fn idle(&mut self, seconds: f64) {
        let until = self.time + seconds;
        while self.time < until {
            self.frame(Vec::new());
        }
    }

    /// The UI tools that reached the app so far.
    fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }

    /// The keys whose press reached the app so far.
    fn seen(&self) -> Vec<Key> {
        self.state.borrow().seen.clone()
    }

    /// The chord the engine is holding.
    fn pending(&self) -> Option<String> {
        self.chrome
            .pending_keys()
            .map(|seq: KeySeq| seq.to_string())
    }
}

#[tokio::test]
async fn a_bound_key_runs_its_tool_and_never_reaches_the_app() {
    let mut f = Fixture::new();
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"]);
    assert_eq!(f.seen(), [], "the keymap swallowed j");
}

#[tokio::test]
async fn a_chord_runs_on_its_last_key() {
    let mut f = Fixture::new();
    f.key(Key::G);
    assert_eq!(f.calls(), Vec::<String>::new(), "g alone runs nothing");
    assert_eq!(f.pending().as_deref(), Some("g"));
    assert_eq!(f.seen(), [], "the chord's first key is swallowed too");

    f.key(Key::G);
    assert_eq!(f.calls(), ["fake_first"]);
    assert_eq!(f.pending(), None);
}

#[tokio::test]
async fn a_chord_lapses_after_its_timeout() {
    let mut f = Fixture::new();
    f.key(Key::G);
    f.idle(CHORD_TIMEOUT + 0.1);
    assert_eq!(f.pending(), None);

    // The next g starts a new chord rather than finishing the old one.
    f.key(Key::G);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.pending().as_deref(), Some("g"));
}

#[tokio::test]
async fn a_chord_broken_by_a_bound_key_runs_that_key() {
    let mut f = Fixture::new();
    f.key(Key::G);
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"]);
    assert_eq!(f.pending(), None);
}

#[tokio::test]
async fn a_focused_text_field_keeps_its_keys() {
    let mut f = Fixture::new();
    f.state.borrow_mut().show_field = true;
    f.frame(Vec::new());
    f.egui_ctx.memory_mut(|m| m.request_focus(field_id()));
    f.frame(Vec::new());

    f.press(Modifiers::NONE, Key::J, Some("j"));
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.state.borrow().text, "j", "j typed into the field");
}

#[tokio::test]
async fn an_unavailable_tool_lets_its_key_through() {
    let mut f = Fixture::new();
    f.state.borrow_mut().gated_available = false;
    f.key(Key::X);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.seen(), [Key::X]);

    f.state.borrow_mut().gated_available = true;
    f.key(Key::X);
    assert_eq!(f.calls(), ["fake_gated"]);
    assert_eq!(f.seen(), [Key::X], "the available one was swallowed");
}

#[tokio::test]
async fn an_unbound_key_reaches_the_app() {
    let mut f = Fixture::new();
    // `k` is bound, but only in a scope the app doesn't report.
    f.key(Key::K);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.seen(), [Key::K]);
}

#[tokio::test]
async fn a_modified_key_matches_only_a_modified_binding() {
    let mut f = Fixture::new();
    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::J, None);
    assert_eq!(f.calls(), Vec::<String>::new(), "Ctrl+j is not j");
    assert_eq!(f.seen(), [Key::J]);

    f.key(Key::F);
    assert_eq!(f.calls(), Vec::<String>::new(), "f is not Ctrl+f");

    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::F, None);
    assert_eq!(f.calls(), ["fake_page"]);
    assert_eq!(f.seen(), [Key::J, Key::F]);
}
