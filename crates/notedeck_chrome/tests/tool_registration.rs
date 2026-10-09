use std::cell::RefCell;
use std::rc::Rc;

use notedeck::{
    App, AppAction, AppResponse, AppTool, Notedeck, RegisteredTool, ToolContext, ToolInvocation,
    ToolKind, ToolOutcome, ToolSpec,
};
use notedeck_chrome::{Chrome, NotedeckApp};

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
