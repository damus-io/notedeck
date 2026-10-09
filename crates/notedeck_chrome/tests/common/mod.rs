//! A headless chrome in front of a fake app, for driving the keymap through
//! real chrome frames: the app reports the `fake` key scope and advertises
//! [`TOOLS`] as UI tools; each test loads a keymap and presses keys.

#![allow(dead_code)] // each test binary uses its own part of the fixture

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use egui::{Event, Key, Modifiers};
use notedeck::{
    App, AppContext, AppResponse, AppTool, Notedeck, RegisteredTool, ToolContext, ToolOutcome,
    ToolSpec,
};
use notedeck_chrome::keymap::{self, KeySeq};
use notedeck_chrome::{Chrome, NotedeckApp};

/// The UI tools [`FakeApp`] advertises.
pub const TOOLS: [&str; 4] = ["fake_down", "fake_first", "fake_page", "fake_gated"];

/// The data tool [`FakeApp`] contributes: it sends its args to the test.
pub const DATA_TOOL: &str = "fake_mark";

/// The id of the text field [`FakeApp`] draws when asked to.
pub fn field_id() -> egui::Id {
    egui::Id::unique("keymap_engine_field")
}

/// What the fake app saw and did, shared with the test.
pub struct FakeState {
    /// UI tools that reached the app, in order.
    pub calls: Vec<String>,
    /// Keys whose press reached the app's render.
    pub seen: Vec<Key>,
    /// Whether `fake_gated` is available.
    pub gated_available: bool,
    /// Whether to draw a text field.
    pub show_field: bool,
    /// The text field's contents.
    pub text: String,
    /// What the app's view is on, its [`App::current_ref`].
    pub current: Option<String>,
    /// The args each [`DATA_TOOL`] call ran with, in order.
    pub marked: mpsc::Receiver<serde_json::Value>,
    /// The room the app had to lay out in on its last render.
    pub area: Option<egui::Rect>,
}

/// [`DATA_TOOL`]: sends its args to the test, which runs it on another
/// thread's terms (`Send + Sync`), so through a channel rather than the
/// shared state.
struct FakeMark {
    sent: mpsc::Sender<serde_json::Value>,
}

impl AppTool for FakeMark {
    type Args = serde_json::Value;
    type Output = serde_json::Value;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(DATA_TOOL, "A fake data tool.", Vec::new())
    }

    fn call(&self, _cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        self.sent.send(args).map_err(|e| e.to_string())?;
        Ok(serde_json::Value::Null)
    }
}

/// An app in the `fake` scope with [`TOOLS`] as UI tools and [`DATA_TOOL`]
/// as a data tool.
struct FakeApp {
    state: Rc<RefCell<FakeState>>,
    /// Where [`DATA_TOOL`] sends.
    sent: mpsc::Sender<serde_json::Value>,
    /// [`FakeState::current`], copied each update so it can be lent out.
    current: Option<String>,
}

impl App for FakeApp {
    fn update(&mut self, _ctx: &mut AppContext<'_>) {
        self.current.clone_from(&self.state.borrow().current);
    }

    fn tools(&self) -> Vec<RegisteredTool> {
        vec![RegisteredTool::new(FakeMark {
            sent: self.sent.clone(),
        })]
    }

    fn current_ref(&self) -> Option<&str> {
        self.current.as_deref()
    }

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
        state.area = Some(ui.available_rect_before_wrap());
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

/// A headless chrome with [`FakeApp`] in front and a test keymap loaded.
pub struct Fixture {
    _dir: tempfile::TempDir,
    pub egui_ctx: egui::Context,
    pub notedeck: Notedeck,
    pub chrome: Chrome,
    pub state: Rc<RefCell<FakeState>>,
    /// The input time of the next frame.
    pub time: f64,
    /// Every piece of text the last frame painted.
    pub drawn: Vec<String>,
}

impl Fixture {
    pub fn new(keymap_src: &str) -> Self {
        Self::with_args(keymap_src, &[])
    }

    /// A fixture signed in as the account with this secret `nsec` (hex), so
    /// the chrome can read and write its private notes.
    pub fn signed_in(keymap_src: &str, nsec: &str) -> Self {
        Self::with_args(keymap_src, &["--nsec", nsec])
    }

    /// A fixture whose notedeck also got `extra` on its command line.
    fn with_args(keymap_src: &str, extra: &[&str]) -> Self {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = ["notedeck-test", "--testrunner"]
            .iter()
            .chain(extra)
            .map(|arg| arg.to_string())
            .collect();
        let egui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
        {
            let app_ctx = notedeck.app_context();
            app_ctx.settings.complete_welcome();
            app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
        }
        notedeck.setup(&egui_ctx);
        let (sent, marked) = mpsc::channel();
        let state = Rc::new(RefCell::new(FakeState {
            calls: Vec::new(),
            seen: Vec::new(),
            gated_available: true,
            show_field: false,
            text: String::new(),
            current: None,
            marked,
            area: None,
        }));
        // The fake app alone, as the nav root: switching to it from another
        // app would slide, and a full chrome frame mid-slide trips egui's
        // debug assert on widget ids changing layer.
        let fake = FakeApp {
            state: Rc::clone(&state),
            sent,
            current: None,
        };
        let mut chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [NotedeckApp::Other("fake".to_owned(), Box::new(fake))],
        );
        let update = App::take_tool_update(&mut chrome).expect("the running set changed");
        notedeck.apply_tool_update(update);
        chrome.set_keymap(keymap::parse(keymap_src).expect("test keymap"));

        let mut fixture = Self {
            _dir: dir,
            egui_ctx,
            notedeck,
            chrome,
            state,
            time: 0.0,
            drawn: Vec::new(),
        };
        fixture.frame(Vec::new());
        fixture.frame(Vec::new());
        fixture
    }

    /// One real chrome frame — `update` then `render`, as the host runs them
    /// — with `events` as its input, a sixtieth of a second after the last.
    pub fn frame(&mut self, events: Vec<Event>) {
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

    /// Whether the last frame painted exactly `text` somewhere.
    pub fn drew(&self, text: &str) -> bool {
        self.drawn.iter().any(|drawn| drawn == text)
    }

    /// One frame with `key` pressed (typing `text`, if any) and released.
    pub fn press(&mut self, modifiers: Modifiers, key: Key, text: Option<&str>) {
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
    pub fn key(&mut self, key: Key) {
        self.press(Modifiers::NONE, key, None);
    }

    /// Run idle frames until `seconds` of input time have passed.
    pub fn idle(&mut self, seconds: f64) {
        let until = self.time + seconds;
        while self.time < until {
            self.frame(Vec::new());
        }
    }

    /// The UI tools that reached the app so far.
    pub fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }

    /// The args of the [`DATA_TOOL`] calls since the last take.
    pub fn marked(&self) -> Vec<serde_json::Value> {
        self.state.borrow().marked.try_iter().collect()
    }

    /// The keys whose press reached the app so far.
    pub fn seen(&self) -> Vec<Key> {
        self.state.borrow().seen.clone()
    }

    /// The chord the engine is holding.
    pub fn pending(&self) -> Option<String> {
        self.chrome
            .pending_keys()
            .map(|seq: KeySeq| seq.to_string())
    }
}

/// Push the text of every text shape in `shape` onto `out`.
pub fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
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
