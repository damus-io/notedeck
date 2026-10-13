use std::cell::RefCell;
use std::rc::Rc;

use notedeck::{
    App, AppAction, AppResponse, AppTool, Notedeck, RegisteredTool, ToolContext, ToolInvocation,
    ToolKind, ToolOutcome, ToolSpec,
};
use notedeck_chrome::{Chrome, NotedeckApp, ToolPlace};

struct ToolProvidingApp;

impl App for ToolProvidingApp {
    fn render(&mut self, _ctx: &mut notedeck::AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }

    fn tools(&self) -> Vec<RegisteredTool> {
        vec![RegisteredTool::new(TestTool)]
    }
}

struct TestTool;

#[derive(serde::Deserialize)]
struct TestArgs;

#[derive(serde::Serialize)]
struct TestOutput;

impl AppTool for TestTool {
    type Args = TestArgs;
    type Output = TestOutput;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new("test_tool", "Test tool forwarding", Vec::new())
    }

    fn call(&self, _ctx: &mut ToolContext<'_>, _args: Self::Args) -> Result<Self::Output, String> {
        Ok(TestOutput)
    }
}

#[test]
fn notedeck_app_forwards_inner_app_tools() {
    let app = NotedeckApp::Other("test".to_owned(), Box::new(ToolProvidingApp));

    let names = app
        .tools()
        .into_iter()
        .map(|tool| tool.name())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["test_tool"]);
}

/// The UI tool [`UiToolApp`] advertises.
const UI_TOOL: &str = "test_cursor_down";

/// An app with one UI tool, recording every call that reaches it.
struct UiToolApp {
    calls: Rc<RefCell<Vec<ToolInvocation>>>,
}

impl App for UiToolApp {
    fn render(&mut self, _ctx: &mut notedeck::AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }

    fn ui_tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new(UI_TOOL, "Move the cursor down.", Vec::new())]
    }

    fn call_ui_tool(
        &mut self,
        _ctx: &mut notedeck::AppContext<'_>,
        name: &str,
        args: &serde_json::Value,
    ) -> ToolOutcome {
        self.calls
            .borrow_mut()
            .push(ToolInvocation::new(name, args.clone()));
        ToolOutcome::Data(serde_json::Value::Null)
    }

    fn key_scopes(&self) -> &'static [&'static str] {
        &["test.grid"]
    }
}

#[test]
fn notedeck_app_forwards_inner_app_ui_tools() {
    let app = NotedeckApp::Other(
        "test".to_owned(),
        Box::new(UiToolApp {
            calls: Rc::default(),
        }),
    );

    let names: Vec<_> = app.ui_tools().iter().map(|spec| spec.name()).collect();
    assert_eq!(names, vec![UI_TOOL]);
    assert_eq!(app.key_scopes(), &["test.grid"]);
    assert!(app.ui_tool_available(UI_TOOL), "available by default");
}

/// A running app's UI tool is listed in `tool_specs` tagged `Ui`; calling it
/// through `call_tool` yields a `CallTool` intent, and the chrome hands that
/// to the owning app with its args.
#[tokio::test]
async fn ui_tools_are_listed_and_reach_their_app_through_chrome() {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let egui_ctx = egui::Context::default();
    let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
    let mut chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");

    let calls = Rc::default();
    let slot = chrome.add_app(NotedeckApp::Other(
        "test".to_owned(),
        Box::new(UiToolApp {
            calls: Rc::clone(&calls),
        }),
    ));
    // Opening the app makes it a running app, whose tools the next update
    // collects, as the host does at the top of each frame.
    chrome.set_active(slot.slot() as i32);
    let update = notedeck::App::take_tool_update(&mut chrome).expect("the running set changed");
    // The app's one UI tool, beside the chrome's own.
    let app_tools: Vec<_> = update
        .ui_tools
        .iter()
        .filter(|entry| entry.owner() == notedeck::ToolOwner::App(slot))
        .collect();
    assert_eq!(app_tools.len(), 1);
    assert!(update.ui_tools.iter().any(|entry| entry.spec().name()
        == notedeck_chrome::TOGGLE_KEY_HINTS
        && entry.owner() == notedeck::ToolOwner::Shell));
    notedeck.apply_tool_update(update);

    let mut ctx = notedeck.app_context();
    let spec = ctx
        .tool_specs()
        .into_iter()
        .find(|spec| spec.name() == UI_TOOL)
        .expect("the UI tool is listed");
    assert_eq!(spec.kind(), ToolKind::Ui);

    let tool_args = serde_json::json!({ "count": 3 });
    let ToolOutcome::Intent(AppAction::CallTool(invocation)) = ctx.call_tool(UI_TOOL, &tool_args)
    else {
        panic!("a UI tool call is a CallTool intent");
    };
    assert!(matches!(
        chrome.call_ui_tool(&mut ctx, &invocation),
        ToolOutcome::Data(_)
    ));
    assert_eq!(
        *calls.borrow(),
        vec![ToolInvocation::new(UI_TOOL, tool_args)]
    );
}

/// A fresh host and a headless chrome running `apps`, every one opened and
/// the first in front, with the registry built from them.
struct Host {
    _dir: tempfile::TempDir,
    notedeck: Notedeck,
    chrome: Chrome,
}

impl Host {
    fn new(apps: impl FnOnce(&mut notedeck::AppContext<'_>) -> Vec<NotedeckApp>) -> Self {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let egui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
        let apps = apps(&mut notedeck.app_context());
        let count = apps.len();
        let mut chrome = Chrome::new_headless_with_apps(&mut notedeck, apps);
        // Opening each app makes it a running one; the first ends in front.
        for slot in (0..count).rev() {
            chrome.set_active(slot as i32);
        }
        let update = notedeck::App::take_tool_update(&mut chrome).expect("the running set changed");
        notedeck.apply_tool_update(update);
        Self {
            _dir: dir,
            notedeck,
            chrome,
        }
    }
}

/// Every tool the built-in apps and the chrome register has a human title
/// and a category, so a command bar never lists a snake_case name.
#[cfg(all(feature = "headway", feature = "dave"))]
#[tokio::test]
async fn every_registered_tool_has_a_title_and_a_category() {
    let mut host = Host::new(|ctx| {
        let dave = notedeck_dave::Dave::new(None, ctx.ndb.clone(), ctx.waker.clone(), ctx.path);
        vec![
            NotedeckApp::Other(
                "headway".to_owned(),
                Box::new(notedeck_headway::Headway::new()),
            ),
            NotedeckApp::Other("dave".to_owned(), Box::new(dave)),
        ]
    });
    let ctx = host.notedeck.app_context();
    let entries: Vec<_> = ctx.registries.tools.entries().collect();
    let names: Vec<_> = entries.iter().map(|entry| entry.spec.name()).collect();
    for wanted in [
        "headway_open_list",
        "headway_archive_card",
        "dave_new_agent",
        notedeck_chrome::TOGGLE_KEY_HINTS,
    ] {
        assert!(names.contains(&wanted), "{wanted} is registered: {names:?}");
    }
    for entry in &entries {
        let spec = entry.spec;
        let title = spec.title().unwrap_or_default();
        assert!(!title.trim().is_empty(), "{} has no title", spec.name());
        assert_ne!(title, spec.name(), "{}'s title is its name", spec.name());
        let category = spec.category().unwrap_or_default();
        assert!(
            !category.trim().is_empty(),
            "{} has no category",
            spec.name()
        );
        assert!(entry.owner.is_some(), "{} has no owner", spec.name());
    }
    // Two apps' tools share no title, so a bar's rows are told apart.
    let mut titles: Vec<_> = entries
        .iter()
        .map(|entry| (entry.spec.category(), entry.spec.title()))
        .collect();
    titles.sort();
    let before = titles.len();
    titles.dedup();
    assert_eq!(titles.len(), before, "a category repeats a title");
}

/// The foreground fake app's UI tools.
const FG_OPEN: &str = "fg_open";
const FG_HIDDEN: &str = "fg_hidden";
/// The foreground fake app's data tools: one on its current ref, one not.
const FG_ARCHIVE: &str = "fg_archive";
const FG_LIST: &str = "fg_list";
/// The background fake app's UI tool.
const BG_TOOL: &str = "bg_tool";

/// A data tool for [`FakeApp`], doing nothing; `on_current` gives it a
/// required argument filled from the current ref.
struct FakeData {
    name: &'static str,
    on_current: bool,
}

#[derive(serde::Deserialize)]
struct FakeArgs {}

impl AppTool for FakeData {
    type Args = FakeArgs;
    type Output = ();

    fn spec(&self) -> ToolSpec {
        let args = if self.on_current {
            vec![
                notedeck::ToolArg::new("card", notedeck::ToolArgType::String, "the card")
                    .required(true)
                    .from_current(),
            ]
        } else {
            Vec::new()
        };
        ToolSpec::new(self.name, "a fake data tool", args).titled(self.name)
    }

    fn call(&self, _ctx: &mut ToolContext<'_>, _args: Self::Args) -> Result<(), String> {
        Ok(())
    }
}

/// An app with some UI tools, of which `hidden` are unavailable, some data
/// tools, one key scope and an optional current ref.
struct FakeApp {
    ui: &'static [&'static str],
    hidden: &'static [&'static str],
    data: &'static [&'static str],
    current: Option<&'static str>,
}

impl App for FakeApp {
    fn render(&mut self, _ctx: &mut notedeck::AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }

    fn tools(&self) -> Vec<RegisteredTool> {
        self.data
            .iter()
            .map(|&name| {
                RegisteredTool::new(FakeData {
                    name,
                    on_current: name == FG_ARCHIVE,
                })
            })
            .collect()
    }

    fn ui_tools(&self) -> Vec<ToolSpec> {
        self.ui
            .iter()
            .map(|&name| ToolSpec::new(name, "a fake UI tool", Vec::new()).titled(name))
            .collect()
    }

    fn ui_tool_available(&self, name: &str) -> bool {
        !self.hidden.contains(&name)
    }

    fn key_scopes(&self) -> &'static [&'static str] {
        &["fake"]
    }

    fn current_ref(&self) -> Option<&str> {
        self.current
    }
}

/// The keymap the runnable-tools tests resolve keycaps in.
const FAKE_KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "o"
tool = "fg_open"

[[bind]]
scope = "fake"
keys = "a"
tool = "fg_archive"
args = { card = "{current}" }

[[bind]]
scope = "global"
keys = "?"
tool = "chrome_toggle_key_hints"
"#;

/// A host with a foreground [`FakeApp`] whose current ref is `current`,
/// and a background one, over [`FAKE_KEYMAP`].
fn fake_host(current: Option<&'static str>) -> Host {
    let mut host = Host::new(|_| {
        vec![
            NotedeckApp::Other(
                "fg".to_owned(),
                Box::new(FakeApp {
                    ui: &[FG_OPEN, FG_HIDDEN],
                    hidden: &[FG_HIDDEN],
                    data: &[FG_ARCHIVE, FG_LIST],
                    current,
                }),
            ),
            NotedeckApp::Other(
                "bg".to_owned(),
                Box::new(FakeApp {
                    ui: &[BG_TOOL],
                    hidden: &[],
                    data: &[],
                    current: None,
                }),
            ),
        ]
    });
    let keymap = notedeck_chrome::keymap::parse(FAKE_KEYMAP).expect("parses");
    host.chrome.set_keymap(keymap);
    host
}

/// The runnable tools list what can run now, the app in front's first, then
/// the chrome's, then the app behind's, each with the keys that run it here.
#[tokio::test]
async fn runnable_tools_list_what_can_run_now_in_order_with_keys() {
    let mut host = fake_host(None);
    let ctx = host.notedeck.app_context();
    let runnable = host.chrome.runnable_tools(&ctx.registries.tools);
    let find = |name: &str| runnable.iter().find(|tool| tool.spec.name() == name);

    let places: Vec<_> = runnable.iter().map(|tool| tool.place).collect();
    let mut sorted = places.clone();
    sorted.sort();
    assert_eq!(places, sorted, "listed in place order");

    let open = find(FG_OPEN).expect("an available UI tool is listed");
    assert_eq!(open.place, ToolPlace::Active);
    assert_eq!(open.keys.map(|keys| keys.to_string()).as_deref(), Some("o"));

    assert!(find(FG_HIDDEN).is_none(), "an unavailable UI tool isn't");
    assert!(
        find(FG_ARCHIVE).is_none(),
        "a current-ref tool isn't, without a current ref"
    );

    let list = find(FG_LIST).expect("a plain data tool is listed");
    assert_eq!(list.place, ToolPlace::Active);
    assert_eq!(list.keys, None);

    let hints = find(notedeck_chrome::TOGGLE_KEY_HINTS).expect("the chrome's own are listed");
    assert_eq!(hints.place, ToolPlace::Shell);
    assert_eq!(hints.owner, notedeck::ToolOwner::Shell);
    assert_eq!(
        hints.keys.map(|keys| keys.to_string()).as_deref(),
        Some("?")
    );

    let bg = find(BG_TOOL).expect("an app behind's tools are listed");
    assert_eq!(bg.place, ToolPlace::Background);
}

/// With a current ref, a tool on it is listed with its key.
#[tokio::test]
async fn a_current_ref_tool_is_runnable_on_the_current_ref() {
    let mut host = fake_host(Some("fake:one"));
    let ctx = host.notedeck.app_context();
    let runnable = host.chrome.runnable_tools(&ctx.registries.tools);
    let archive = runnable
        .iter()
        .find(|tool| tool.spec.name() == FG_ARCHIVE)
        .expect("listed while the app has a current ref");
    assert_eq!(archive.place, ToolPlace::Active);
    assert!(archive.spec.needs_current());
    assert_eq!(
        archive.keys.map(|keys| keys.to_string()).as_deref(),
        Some("a")
    );
}
