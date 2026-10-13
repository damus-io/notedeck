//! The Cmd-K command bar through real chrome frames: Cmd-K opens it over the
//! fake app, typing filters a list of rows, the keys move and run the
//! selection, Esc, Cmd-K and a click outside close it, and the keymap stands
//! down while it is open. Opened, it lists the tools that can run now, and a
//! tool with arguments asks for them, one step each.

mod common;

use common::{Fixture, DATA_TOOL, NAME_TOOL, PICK_TOOL};
use egui::{Event, Key, Modifiers, PointerButton};
use notedeck::{CommandAction, CommandItem, ToolInvocation};
use notedeck_chrome::OPEN_COMMAND_BAR;

/// `j` runs `fake_down`, so a test can tell whether the keymap ran, `o`
/// the bar's own open tool, and `b` picks `beta` with `fake_pick`.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "j"
tool = "fake_down"

[[bind]]
scope = "fake"
keys = "o"
tool = "chrome_open_command_bar"

[[bind]]
scope = "fake"
keys = "b"
tool = "fake_pick"
args = { by = "beta" }
"#;

/// A row under `section` that runs the tool `tool`.
fn tool_row(section: &'static str, title: &str, tool: &str) -> CommandItem {
    CommandItem::new(
        section,
        title,
        CommandAction::RunTool(ToolInvocation::new(tool, serde_json::Value::Null)),
    )
}

/// The rows the tests filter: three of the fake app's UI tools and its data
/// tool, under two sections.
fn rows() -> Vec<CommandItem> {
    vec![
        tool_row("Fake", "Move down", "fake_down").with_keycap("j"),
        tool_row("Fake", "Go to first", "fake_first").with_keycap("gg"),
        tool_row("Fake", "Page down", "fake_page"),
        tool_row("Marks", "Mark this", DATA_TOOL).subtitled("a data tool"),
    ]
}

/// Press Cmd-K the way this platform's keyboard reports it: Ctrl sets
/// `command` too off macOS.
fn cmd_k(f: &mut Fixture) {
    f.press(Modifiers::COMMAND | Modifiers::CTRL, Key::K, None);
}

/// Type `text` into whatever has focus, one frame.
fn type_text(f: &mut Fixture, text: &str) {
    f.frame(vec![Event::Text(text.to_owned())]);
}

/// A fixture with the bar open on [`rows`].
fn open_on_rows() -> Fixture {
    let mut f = Fixture::new(KEYMAP);
    cmd_k(&mut f);
    assert!(f.chrome.command_bar().is_open());
    f.chrome.command_bar_mut().set_items(rows());
    f.frame(Vec::new());
    f
}

/// The titles the bar lists, in order.
fn listed(f: &mut Fixture) -> Vec<String> {
    f.chrome
        .command_bar_mut()
        .matches()
        .map(|item| item.title.clone())
        .collect()
}

/// The selected row's title.
fn selected(f: &mut Fixture) -> Option<String> {
    f.chrome
        .command_bar_mut()
        .selected()
        .map(|item| item.title.clone())
}

#[test]
fn cmd_k_opens_it_and_esc_closes_it() {
    let mut f = Fixture::new(KEYMAP);
    assert!(!f.chrome.command_bar().is_open());

    cmd_k(&mut f);
    assert!(f.chrome.command_bar().is_open());
    // egui hides a new area for its first frame, while it measures it.
    f.frame(Vec::new());
    assert!(f.drew("Pick a letter"), "it lists the tools");

    f.key(Key::Escape);
    assert!(!f.chrome.command_bar().is_open());
    assert!(!f.drew("Pick a letter"));
}

#[test]
fn cmd_k_again_closes_it() {
    let mut f = Fixture::new(KEYMAP);
    cmd_k(&mut f);
    cmd_k(&mut f);
    assert!(!f.chrome.command_bar().is_open());
}

#[test]
fn cmd_shift_k_does_not_open_it() {
    let mut f = Fixture::new(KEYMAP);
    f.press(
        Modifiers::COMMAND | Modifiers::CTRL | Modifiers::SHIFT,
        Key::K,
        None,
    );
    assert!(!f.chrome.command_bar().is_open());
}

#[test]
fn it_opens_with_a_text_field_focused() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().show_field = true;
    f.frame(Vec::new());
    f.egui_ctx
        .memory_mut(|m| m.request_focus(common::field_id()));
    f.frame(Vec::new());

    cmd_k(&mut f);
    assert!(f.chrome.command_bar().is_open());
    type_text(&mut f, "x");
    assert_eq!(f.chrome.command_bar().query(), "x");
    assert_eq!(f.state.borrow().text, "", "the app's field typed nothing");

    f.key(Key::Escape);
    f.frame(Vec::new());
    assert_eq!(
        f.egui_ctx.memory(|m| m.focused()),
        Some(common::field_id()),
        "closing gives the app's field its focus back"
    );
}

#[test]
fn the_open_tool_opens_it() {
    assert!(KEYMAP.contains(OPEN_COMMAND_BAR));
    let mut f = Fixture::new(KEYMAP);
    f.press(Modifiers::NONE, Key::O, Some("o"));
    assert!(f.chrome.command_bar().is_open());
    f.frame(Vec::new());
    assert_eq!(
        f.chrome.command_bar().query(),
        "",
        "the key that opened it isn't typed"
    );
}

#[test]
fn it_lists_the_rows_grouped_by_section_in_the_order_given() {
    let mut f = open_on_rows();
    assert_eq!(
        listed(&mut f),
        ["Move down", "Go to first", "Page down", "Mark this"]
    );
    assert!(f.drew("Fake"));
    assert!(f.drew("Marks"));
    assert!(f.drew("Go to first"));
    assert!(f.drew("gg"), "a row's keycap is drawn");
}

#[test]
fn typing_filters_and_ranks_the_rows() {
    let mut f = open_on_rows();
    type_text(&mut f, "down");
    assert_eq!(f.chrome.command_bar().query(), "down");
    assert_eq!(listed(&mut f), ["Move down", "Page down"]);
    assert!(f.drew("Move down"));
    assert!(!f.drew("Go to first"));

    type_text(&mut f, "zz");
    assert!(listed(&mut f).is_empty());
    assert!(f.drew("No results"));
}

#[test]
fn the_best_section_comes_first() {
    let mut f = open_on_rows();
    let mut items = rows();
    items.insert(1, tool_row("Fake", "Bookmark page", "fake_page"));
    f.chrome.command_bar_mut().set_items(items);
    type_text(&mut f, "mark");
    // "mark" starts "Mark this" but is mid-word in "Bookmark page", so the
    // Marks section leads although it was given last.
    assert_eq!(listed(&mut f), ["Mark this", "Bookmark page"]);
}

#[test]
fn the_keymap_stands_down_while_it_is_open() {
    let mut f = open_on_rows();
    f.press(Modifiers::NONE, Key::J, Some("j"));
    assert!(f.calls().is_empty(), "j went to the query, not the keymap");
    assert_eq!(f.chrome.command_bar().query(), "j");

    f.key(Key::Escape);
    f.press(Modifiers::NONE, Key::J, Some("j"));
    assert_eq!(f.calls(), ["fake_down"], "closed, the keymap is back");
}

#[test]
fn arrows_and_ctrl_n_p_move_the_selection_and_enter_runs_it() {
    let mut f = open_on_rows();
    assert_eq!(selected(&mut f).as_deref(), Some("Move down"));

    f.key(Key::ArrowDown);
    assert_eq!(selected(&mut f).as_deref(), Some("Go to first"));
    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::N, None);
    assert_eq!(selected(&mut f).as_deref(), Some("Page down"));
    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::P, None);
    f.key(Key::ArrowUp);
    f.key(Key::ArrowUp);
    assert_eq!(
        selected(&mut f).as_deref(),
        Some("Move down"),
        "the selection stops at the top"
    );
    f.key(Key::ArrowDown);

    f.key(Key::Enter);
    assert_eq!(f.calls(), ["fake_first"]);
    assert!(!f.chrome.command_bar().is_open(), "running a row closes it");
}

#[test]
fn enter_on_a_data_tool_runs_it_at_once() {
    let mut f = open_on_rows();
    type_text(&mut f, "mark");
    f.key(Key::Enter);
    assert_eq!(f.marked(), [serde_json::Value::Null]);
    assert!(f.calls().is_empty());
}

#[test]
fn enter_with_nothing_matched_just_closes_it() {
    let mut f = open_on_rows();
    type_text(&mut f, "zz");
    f.key(Key::Enter);
    assert!(!f.chrome.command_bar().is_open());
    assert!(f.calls().is_empty());
}

#[test]
fn a_click_outside_closes_it() {
    let mut f = open_on_rows();
    let corner = egui::pos2(10.0, 790.0);
    f.frame(vec![Event::PointerMoved(corner)]);
    let click = |pressed| Event::PointerButton {
        pos: corner,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    f.frame(vec![click(true)]);
    f.frame(vec![click(false)]);
    assert!(!f.chrome.command_bar().is_open());
    assert!(f.calls().is_empty(), "the click never reached the app");
}

#[test]
fn the_bars_keys_never_reach_the_app() {
    let mut f = open_on_rows();
    for key in [Key::ArrowDown, Key::ArrowUp, Key::Enter] {
        f.key(key);
    }
    cmd_k(&mut f);
    f.key(Key::Escape);
    let seen = f.seen();
    for key in [
        Key::ArrowDown,
        Key::ArrowUp,
        Key::Enter,
        Key::K,
        Key::Escape,
    ] {
        assert!(!seen.contains(&key), "{key:?} reached the app: {seen:?}");
    }
}

/// An app with a few lines of text, for the backdrop to dim, and two tools
/// for the bar to list, one asking what to group by.
struct Backdrop;

impl notedeck::App for Backdrop {
    fn render(
        &mut self,
        _ctx: &mut notedeck::AppContext,
        ui: &mut egui::Ui,
    ) -> notedeck::AppResponse {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.heading("Board: notedeck");
                for line in [
                    "chrome: Cmd-K command bar overlay and shared fuzzy matcher",
                    "chrome: Cmd-K action bar lists and runs app tools",
                    "search provider: headway boards and cards",
                ] {
                    ui.label(line);
                }
            });
        notedeck::AppResponse::none()
    }

    fn ui_tools(&self) -> Vec<notedeck::ToolSpec> {
        use notedeck::{ToolArg, ToolArgType, ToolSpec};
        vec![
            ToolSpec::new(
                "backdrop_group",
                "Group the board's cards.",
                vec![ToolArg::new(
                    "by",
                    ToolArgType::Enum(vec!["next", "status", "priority", "label", "none"]),
                    "What to group by.",
                )
                .default(serde_json::json!("next"))],
            )
            .titled("Group cards by")
            .filed_under("Board"),
            ToolSpec::new("backdrop_new", "Add a card.", Vec::new())
                .titled("New card")
                .filed_under("Board"),
        ]
    }
}

/// A chrome in front of [`Backdrop`], drawn as the host draws it.
struct Snapshot {
    notedeck: notedeck::Notedeck,
    chrome: notedeck_chrome::Chrome,
    _dir: tempfile::TempDir,
}

/// The rows the snapshots list: what the action bar will give it, roughly.
fn snapshot_rows() -> Vec<CommandItem> {
    vec![
        tool_row("Headway", "Next board", "headway_board_next").with_keycap("]b"),
        tool_row("Headway", "Previous board", "headway_board_prev").with_keycap("[b"),
        tool_row("Headway", "Open board picker", "headway_board_picker").with_keycap("gb"),
        tool_row("Headway", "New card", "headway_new_card")
            .subtitled("in the focused group")
            .with_keycap("c"),
        tool_row("Dave", "New agent", "dave_new_agent"),
        tool_row("Dave", "Rename agent", "dave_rename_agent"),
        tool_row("Chrome", "Toggle key hints", "chrome_toggle_key_hints").with_keycap("?"),
        tool_row("Chrome", "Edit keymap", "chrome_edit_keymap"),
    ]
}

/// A harness on a [`Snapshot`] with the bar open on [`snapshot_rows`] and
/// `query` typed.
fn snapshot_harness(query: &str) -> egui_kittest::Harness<'static, Snapshot> {
    let mut harness = backdrop_harness();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
    harness.run_steps(2);
    let bar = harness.state_mut().chrome.command_bar_mut();
    bar.set_items(snapshot_rows());
    bar.set_query(query);
    let _ = harness.run_ok();
    harness
}

/// A harness on a [`Snapshot`], its tools registered, the bar closed.
fn backdrop_harness() -> egui_kittest::Harness<'static, Snapshot> {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
    {
        let app_ctx = notedeck.app_context();
        app_ctx.settings.complete_welcome();
        app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
    }
    let mut chrome = notedeck_chrome::Chrome::new_headless_with_apps(
        &mut notedeck,
        [notedeck_chrome::NotedeckApp::Other(
            "Backdrop".to_owned(),
            Box::new(Backdrop),
        )],
    );
    let update = notedeck::App::take_tool_update(&mut chrome).expect("the running set changed");
    notedeck.apply_tool_update(update);
    let state = Snapshot {
        notedeck,
        chrome,
        _dir: dir,
    };
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .renderer(notedeck::software_renderer())
        .build_ui_state(
            |ui, state: &mut Snapshot| {
                notedeck::test_harness::app_frame(ui, |ui| {
                    let mut app_ctx = state.notedeck.app_context();
                    notedeck::App::update(&mut state.chrome, &mut app_ctx);
                    notedeck::App::render(&mut state.chrome, &mut app_ctx, ui);
                });
            },
            state,
        )
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_empty() {
    let mut harness = snapshot_harness("");
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_empty");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_filtered() {
    let mut harness = snapshot_harness("bo");
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_filtered");
}

/// A fixture with the bar open on the tools, as Cmd-K opens it.
fn open_on_tools() -> Fixture {
    let mut f = Fixture::new(KEYMAP);
    cmd_k(&mut f);
    f.frame(Vec::new());
    f
}

/// The UI tools that reached the fake app, with their args.
fn calls_with_args(f: &Fixture) -> Vec<ToolInvocation> {
    let state = f.state.borrow();
    state
        .calls
        .iter()
        .zip(&state.call_args)
        .map(|(name, args)| ToolInvocation::new(name.clone(), args.clone()))
        .collect()
}

#[test]
fn it_lists_the_tools_that_can_run_now_active_apps_first() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().gated_available = false;
    cmd_k(&mut f);
    f.frame(Vec::new());
    let listed = listed(&mut f);
    let at = |title: &str| listed.iter().position(|t| t == title);

    for tool in ["fake_down", "Pick a letter", "Name it", DATA_TOOL] {
        assert!(at(tool).is_some(), "{tool} is listed: {listed:?}");
    }
    assert_eq!(at("fake_gated"), None, "an unavailable tool isn't");
    assert_eq!(at("fake_answer"), None, "a read-only tool isn't");
    assert_eq!(at("Open command bar"), None, "it is open already");
    assert_eq!(at("Save keymap"), None, "its TOML is the editor's to send");
    let chrome = at("Toggle key hints").expect("the chrome's tools are listed");
    assert!(
        at("fake_down").unwrap() < chrome,
        "the app in front comes first: {listed:?}"
    );
    assert!(f.drew("Fake"), "the tools are under their category");
    assert!(f.drew("j"), "a tool's keys are drawn");
}

#[test]
fn typing_matches_a_tools_name_and_description_too() {
    let mut f = open_on_tools();
    type_text(&mut f, "letter");
    assert_eq!(listed(&mut f), ["Pick a letter"]);

    f.chrome.command_bar_mut().set_query(NAME_TOOL);
    assert_eq!(listed(&mut f), ["Name it"], "by its name");

    f.chrome.command_bar_mut().set_query("names current");
    assert_eq!(listed(&mut f), ["Name it"], "by its description's words");
}

#[test]
fn an_enum_argument_is_picked_from_its_values() {
    let mut f = open_on_tools();
    type_text(&mut f, "pick");
    f.key(Key::Enter);
    assert!(f.chrome.command_bar().is_open(), "it asks for the argument");
    assert!(f.calls().is_empty(), "nothing ran yet");
    assert_eq!(f.chrome.command_bar().prompt(), Some("Pick a letter"));
    assert_eq!(f.chrome.command_bar().query(), "", "the query starts over");
    assert_eq!(listed(&mut f), ["Alpha", "Beta"]);
    let subtitles: Vec<_> = f
        .chrome
        .command_bar_mut()
        .matches()
        .map(|item| item.subtitle.clone())
        .collect();
    assert_eq!(
        subtitles,
        [Some("default".to_owned()), None],
        "the default value is marked"
    );
    assert!(f.drew("b"), "a value's keys are drawn");

    f.key(Key::ArrowDown);
    f.key(Key::Enter);
    assert_eq!(
        calls_with_args(&f),
        [ToolInvocation::new(
            PICK_TOOL,
            serde_json::json!({ "by": "beta" })
        )]
    );
    assert!(!f.chrome.command_bar().is_open());
}

#[test]
fn backspace_in_an_empty_query_steps_back_out() {
    let mut f = open_on_tools();
    type_text(&mut f, "pick");
    f.key(Key::Enter);
    type_text(&mut f, "b");
    f.key(Key::Backspace);
    assert_eq!(
        f.chrome.command_bar().prompt(),
        Some("Pick a letter"),
        "a backspace with text typed edits it"
    );
    assert_eq!(f.chrome.command_bar().query(), "");

    f.key(Key::Backspace);
    assert_eq!(f.chrome.command_bar().prompt(), None);
    assert_eq!(f.chrome.command_bar().query(), "pick", "as it was left");
    assert_eq!(listed(&mut f), ["Pick a letter"]);
    assert!(f.chrome.command_bar().is_open());
}

#[test]
fn a_text_argument_is_the_query_and_current_fills_its_own() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().current = Some("fake:7".to_owned());
    f.frame(Vec::new());
    cmd_k(&mut f);
    type_text(&mut f, "name it");
    f.key(Key::Enter);
    assert_eq!(f.chrome.command_bar().prompt(), Some("Name it"));
    f.frame(Vec::new());
    assert!(f.drew("Press Enter to run it"));

    f.key(Key::Enter);
    assert!(
        f.chrome.command_bar().is_open(),
        "an empty text runs nothing"
    );
    assert!(f.calls().is_empty());

    type_text(&mut f, "Bob");
    f.key(Key::Enter);
    assert_eq!(
        calls_with_args(&f),
        [ToolInvocation::new(
            NAME_TOOL,
            serde_json::json!({ "name": "Bob", "target": "fake:7" })
        )]
    );
    assert!(!f.chrome.command_bar().is_open());
}

#[test]
fn the_tools_run_lately_come_first() {
    let mut f = open_on_tools();
    type_text(&mut f, "fake_page");
    f.key(Key::Enter);
    assert_eq!(f.calls(), ["fake_page"]);

    cmd_k(&mut f);
    f.frame(Vec::new());
    assert_eq!(
        listed(&mut f).first().map(String::as_str),
        Some("fake_page")
    );
    assert!(f.drew("Recent"));
    assert_eq!(
        listed(&mut f).iter().filter(|t| *t == "fake_page").count(),
        1,
        "listed once"
    );
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_tools() {
    let mut harness = backdrop_harness();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
    let _ = harness.run_ok();
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_tools");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_arg_step() {
    let mut harness = backdrop_harness();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::K);
    harness.run_steps(2);
    harness
        .state_mut()
        .chrome
        .command_bar_mut()
        .set_query("group");
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    let _ = harness.run_ok();
    assert_eq!(
        harness.state().chrome.command_bar().prompt(),
        Some("Group cards by")
    );
    harness.snapshot("command_bar_arg_step");
}
