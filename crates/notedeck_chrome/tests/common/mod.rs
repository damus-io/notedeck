//! A headless chrome in front of a fake app, for driving the keymap through
//! real chrome frames: the app reports the `fake` key scope and advertises
//! [`TOOLS`] as UI tools; each test loads a keymap and presses keys.

#![allow(dead_code)] // each test binary uses its own part of the fixture

use std::cell::RefCell;
use std::rc::Rc;

use egui::{Event, Key, Modifiers};
use notedeck::{App, AppContext, AppResponse, Notedeck, ToolOutcome, ToolSpec};
use notedeck_chrome::keymap::{self, KeySeq};
use notedeck_chrome::{Chrome, NotedeckApp};

/// The UI tools [`FakeApp`] advertises.
pub const TOOLS: [&str; 4] = ["fake_down", "fake_first", "fake_page", "fake_gated"];

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
