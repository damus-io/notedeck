//! The Cmd-K command bar through real chrome frames: Cmd-K opens it over the
//! fake app, typing filters a list of rows, the keys move and run the
//! selection, Esc, Cmd-K and a click outside close it, and the keymap stands
//! down while it is open. Opened, it lists the tools that can run now, and a
//! tool with arguments asks for them, one step each.

mod common;

use common::{bar_key, Fixture, DATA_TOOL, NAME_TOOL, PICK_TOOL};
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

/// Press the bar's key, Cmd-K on macOS and Ctrl-Space elsewhere, the way
/// this platform's keyboard reports it.
fn cmd_k(f: &mut Fixture) {
    let open = bar_key(&f.egui_ctx);
    f.press(open.modifiers, open.logical_key, None);
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
fn the_key_with_shift_does_not_open_it() {
    let mut f = Fixture::new(KEYMAP);
    let open = bar_key(&f.egui_ctx);
    f.press(open.modifiers | Modifiers::SHIFT, open.logical_key, None);
    assert!(!f.chrome.command_bar().is_open());
}

/// Off macOS the bar's key is Ctrl-Space, not Ctrl-K: there Cmd is Ctrl,
/// and the chrome takes its key before any app, so Ctrl-K would never reach
/// an app that binds it, like Dave's previous agent.
#[test]
fn ctrl_k_off_macos_reaches_the_app() {
    let mut f = Fixture::new(KEYMAP);
    if f.egui_ctx.os() == egui::os::OperatingSystem::Mac {
        return;
    }
    f.press(Modifiers::COMMAND | Modifiers::CTRL, Key::K, None);
    assert!(!f.chrome.command_bar().is_open());
    assert!(f.seen().contains(&Key::K), "the app saw it: {:?}", f.seen());
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
        bar_key(&f.egui_ctx).logical_key,
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

    /// Cards and a board, as Headway's provider will find them, for a
    /// query for cards only, so the other snapshots list tools alone.
    fn search(
        &mut self,
        _ctx: &mut notedeck::AppContext<'_>,
        query: &notedeck::SearchQuery<'_>,
        out: &mut notedeck::SearchHits,
    ) {
        use notedeck::{OpenUri, SearchHit};
        if query.text != "card" {
            return;
        }
        let hits = [
            SearchHit::new(
                "chrome: Cmd-K command bar overlay and shared fuzzy matcher",
                OpenUri::new("headway:notedeck/fury-excuse-accident"),
                0,
            )
            .kind("Card")
            .subtitled("In Review"),
            SearchHit::new(
                "chrome: Cmd-K action bar lists and runs app tools",
                OpenUri::new("headway:notedeck/save-jump-disease"),
                0,
            )
            .kind("Card")
            .subtitled("In Review"),
            SearchHit::new("notedeck", OpenUri::new("headway:notedeck"), -40)
                .kind("Board")
                .subtitled("matched a card's body"),
        ];
        for hit in hits {
            out.push(hit);
        }
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
/// `query` typed, in dark mode.
fn snapshot_harness(query: &str) -> egui_kittest::Harness<'static, Snapshot> {
    themed_snapshot_harness(query, egui::Theme::Dark)
}

/// [`snapshot_harness`] in `theme`.
fn themed_snapshot_harness(
    query: &str,
    theme: egui::Theme,
) -> egui_kittest::Harness<'static, Snapshot> {
    let mut harness = themed_backdrop_harness(theme);
    let open = bar_key(&harness.ctx);
    harness.key_press_modifiers(open.modifiers, open.logical_key);
    harness.run_steps(2);
    let bar = harness.state_mut().chrome.command_bar_mut();
    bar.set_items(snapshot_rows());
    bar.set_query(query);
    let _ = harness.run_ok();
    harness
}

/// A harness on a [`Snapshot`], its tools registered, the bar closed, in
/// dark mode.
fn backdrop_harness() -> egui_kittest::Harness<'static, Snapshot> {
    themed_backdrop_harness(egui::Theme::Dark)
}

/// [`backdrop_harness`] in `theme`.
fn themed_backdrop_harness(theme: egui::Theme) -> egui_kittest::Harness<'static, Snapshot> {
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
        .with_theme(theme)
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

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_filtered_light() {
    let mut harness = themed_snapshot_harness("bo", egui::Theme::Light);
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_filtered_light");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_search() {
    let mut harness = snapshot_harness("card");
    let _ = harness.run_ok();
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_search");
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
    let open = bar_key(&harness.ctx);
    harness.key_press_modifiers(open.modifiers, open.logical_key);
    let _ = harness.run_ok();
    assert!(harness.state().chrome.command_bar().is_open());
    harness.snapshot("command_bar_tools");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_command_bar_arg_step() {
    let mut harness = backdrop_harness();
    let open = bar_key(&harness.ctx);
    harness.key_press_modifiers(open.modifiers, open.logical_key);
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

// ---------------------------------------------------------------------------
// Rich rows: hits that stand for a profile or a note
// ---------------------------------------------------------------------------

/// Where the profile's picture is said to live. Nothing answers there: the
/// picture is put in the disk cache under this url first, so it loads the
/// same on every machine and the snapshot never waits on the network.
const RICH_PFP_URL: &str = "http://127.0.0.1:9/quokka.png";

/// An app that answers the query `rich` with `hits`, and draws notes with
/// the real renderers: Columns' for a kind-1 note, Headway's for a card.
struct Rich {
    /// What it answers `rich` with.
    hits: Vec<notedeck::SearchHit>,
    /// Where Headway's renderers come from.
    headway: notedeck_headway::Headway,
}

impl notedeck::App for Rich {
    fn render(
        &mut self,
        _ctx: &mut notedeck::AppContext,
        _ui: &mut egui::Ui,
    ) -> notedeck::AppResponse {
        notedeck::AppResponse::none()
    }

    fn search(
        &mut self,
        _ctx: &mut notedeck::AppContext<'_>,
        query: &notedeck::SearchQuery<'_>,
        out: &mut notedeck::SearchHits,
    ) {
        if query.text != "rich" {
            return;
        }
        for hit in &self.hits {
            out.push(hit.clone());
        }
    }

    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        let mut renderers: Vec<Box<dyn notedeck::KindRenderer>> =
            vec![Box::new(notedeck_columns::NoteKindRenderer)];
        renderers.extend(notedeck::App::kind_renderers(&self.headway));
        renderers
    }
}

/// The host, with the chrome in front of [`Rich`] set as its app, drawn
/// through [`notedeck::Notedeck::tick`] so media jobs deliver the picture.
struct RichState {
    notedeck: notedeck::Notedeck,
    /// Whether the first frame, which installs the fonts, has run.
    set_up: bool,
    _dir: tempfile::TempDir,
}

/// Sign a note of `kind` with `content` by `kp` at `created_at`.
fn signed(
    kp: &nostrdb_net::FullKeypair,
    kind: u32,
    content: &str,
    created_at: u64,
) -> nostrdb::Note<'static> {
    nostrdb::NoteBuilder::new()
        .kind(kind)
        .content(content)
        .created_at(created_at)
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("note")
}

/// A profile with [`RICH_PFP_URL`] as its picture and a note by it, the
/// demo board seeded as its own, and the picture in the disk cache.
/// Returns the note's id.
async fn seed_rich(
    notedeck: &mut notedeck::Notedeck,
    kp: &nostrdb_net::FullKeypair,
) -> nostrdb_net::NoteId {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let profile = signed(
        kp,
        0,
        &format!(
            r#"{{"name":"quokka","display_name":"Quentin Quokka","picture":"{RICH_PFP_URL}"}}"#
        ),
        now - 86_400,
    );
    // Two hours old, so it reads "2h" whenever the snapshot is made.
    let note = signed(
        kp,
        1,
        "Cmd-K draws what it finds now: a profile with its picture, a note as \
         the note it is, and a card with the status it has on its board, \
         live, the way every other surface draws them.",
        now - 2 * 3_600,
    );
    let note_id = nostrdb_net::NoteId::new(*note.id());

    let app_ctx = notedeck.app_context();
    let ndb = &*app_ctx.ndb;
    let sub = ndb
        .subscribe(&[nostrdb::Filter::new().authors([kp.pubkey.bytes()]).build()])
        .expect("subscribe");
    let mut written = 0;
    for event in [&profile, &note] {
        let frame = nostrdb_net::ClientMessage::event(event)
            .expect("event")
            .to_json()
            .expect("json");
        ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
            .expect("ingest");
        written += 1;
    }
    written += notedeck_headway::store::seed_demo_board(
        ndb,
        &kp.pubkey,
        &kp.secret_key.secret_bytes(),
        notedeck_headway::store::BOARD_ID,
        1_700_000_000,
        &mut notedeck_headway::store::NoPublish,
    );
    let written = u32::try_from(written).expect("a demo board's worth of notes");
    ndb.wait_for_all_notes_within(sub, written, Duration::from_secs(10))
        .await
        .expect("the seed lands");

    // A quokka-ish brown disc on a warm ground, so a picture that loads is
    // told from the grey circle drawn while one doesn't.
    let size = 128;
    let mut pixels = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - 64.0, y as f32 - 70.0);
            let face = dx * dx / 1.4 + dy * dy < 46.0 * 46.0;
            pixels.push(if face {
                egui::Color32::from_rgb(158, 112, 72)
            } else {
                egui::Color32::from_rgb(246, 196, 120)
            });
        }
    }
    let image = egui::ColorImage::new([size, size], pixels);
    notedeck::MediaCache::write(
        &app_ctx.img_cache.static_imgs.cache_dir,
        RICH_PFP_URL,
        image,
    )
    .expect("the picture goes in the disk cache");
    note_id
}

/// A card of the demo board: the issue note it was made by and its title.
struct DemoCard {
    id: nostrdb_net::NoteId,
    title: String,
}

/// The demo board's cards titled `titles`, in that order, each titled as
/// `renderers` (Headway's) read it now.
fn demo_cards(
    notedeck: &mut notedeck::Notedeck,
    renderers: &notedeck::KindRendererRegistry,
    titles: &[&str],
) -> Vec<DemoCard> {
    let app_ctx = notedeck.app_context();
    let ndb = &*app_ctx.ndb;
    let txn = nostrdb::Transaction::new(ndb).expect("txn");
    let filter = nostrdb::Filter::new()
        .kinds([notedeck_headway::event::KIND_ISSUE as u64])
        .build();
    let mut cards: Vec<DemoCard> = ndb
        .query(&txn, &[filter], 64)
        .expect("query")
        .into_iter()
        .filter_map(|result| {
            Some(DemoCard {
                id: nostrdb_net::NoteId::new(*result.note.id()),
                title: renderers.title(ndb, &txn, &result.note, None)?,
            })
        })
        .collect();
    cards.retain(|card| titles.contains(&card.title.as_str()));
    cards.sort_by_key(|card| titles.iter().position(|t| *t == card.title));
    cards
}

/// What [`Rich`] answers `rich` with: the profile, its note, `cards`, and a
/// note the db doesn't have, which draws as text. Scored in that order, so
/// the bar lists them so.
fn rich_hits(
    kp: &nostrdb_net::FullKeypair,
    note: nostrdb_net::NoteId,
    cards: &[DemoCard],
) -> Vec<notedeck::SearchHit> {
    use notedeck::{HitPreview, OpenUri, SearchHit};
    let npub = kp.pubkey.npub().expect("npub");
    let mut hits = vec![
        SearchHit::new("Quentin Quokka", OpenUri::new(format!("nostr:{npub}")), 100)
            .kind("Profile")
            .subtitled("@quokka")
            .previewed(HitPreview::Profile(kp.pubkey)),
        SearchHit::new(
            "Cmd-K draws what it finds now",
            OpenUri::new(format!("nostr:{}", note.to_bech().expect("bech"))),
            90,
        )
        .kind("Note")
        .subtitled("Quentin Quokka")
        .previewed(HitPreview::Note(note)),
    ];
    for (rank, card) in cards.iter().enumerate() {
        hits.push(
            SearchHit::new(
                card.title.clone(),
                OpenUri::new(format!("headway:demo/card-{rank}")),
                80 - rank as i32,
            )
            .kind("Card")
            .subtitled("Demo")
            .previewed(HitPreview::Note(card.id)),
        );
    }
    hits.push(
        SearchHit::new(
            "a note this db has never seen",
            OpenUri::new("nostr:note1missing"),
            0,
        )
        .kind("Note")
        .subtitled("drawn as text")
        .previewed(HitPreview::Note(nostrdb_net::NoteId::new([7; 32]))),
    );
    hits
}

/// One frame of [`RichState`]: the first installs the fonts, as
/// `Notedeck::setup` does for the app; the rest tick the host.
fn rich_frame(ui: &mut egui::Ui, state: &mut RichState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if !state.set_up {
            let ctx = ui.ctx().clone();
            state.notedeck.setup(&ctx);
            ctx.global_style_mut(|s| s.animation_time = 0.0);
            state.set_up = true;
            return;
        }
        state.notedeck.tick(ui);
    });
}

/// Whether the profile's picture has loaded from the disk cache.
fn rich_pfp_loaded(harness: &mut egui_kittest::Harness<'static, RichState>) -> bool {
    let ctx = harness.ctx.clone();
    let app_ctx = harness.state_mut().notedeck.app_context();
    let state = app_ctx.img_cache.textures.static_image.get_or_request(
        app_ctx.media_jobs.sender(),
        &ctx,
        RICH_PFP_URL,
        notedeck::media::images::ImageType::Profile(128),
    );
    matches!(state, notedeck::TextureState::Loaded(_))
}

/// The bar on a query whose hits stand for things: a profile draws with its
/// picture, a note through Columns' renderer as its author and text in a
/// taller row, cards through Headway's with their live status, each with
/// its kind and subtitle at the right, and a note not in the db as the
/// plain line every row was before.
#[tokio::test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
async fn snapshot_command_bar_rich_rows() {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let kp = nostrdb_net::FullKeypair::generate();
    let args: Vec<String> = vec![
        "notedeck-test".into(),
        "--testrunner".into(),
        "--nsec".into(),
        kp.secret_key.to_secret_hex(),
    ];
    let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
    {
        let app_ctx = notedeck.app_context();
        app_ctx.settings.complete_welcome();
        app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
    }
    let note = seed_rich(&mut notedeck, &kp).await;

    let headway = notedeck_headway::Headway::new();
    let mut headway_renderers = notedeck::KindRendererRegistry::default();
    for renderer in notedeck::App::kind_renderers(&headway) {
        headway_renderers.register(renderer);
    }
    // One from each of three columns, so each draws its own status.
    let titles = [
        "Drag-and-drop between columns",
        "Card detail / comments view",
        "Scaffold the Headway app crate",
    ];
    let cards = demo_cards(&mut notedeck, &headway_renderers, &titles);
    assert_eq!(cards.len(), titles.len(), "the demo board's cards fold");
    let rich = Rich {
        hits: rich_hits(&kp, note, &cards),
        headway,
    };
    let chrome = notedeck_chrome::Chrome::new_headless_with_apps(
        &mut notedeck,
        [notedeck_chrome::NotedeckApp::Other(
            "Rich".to_owned(),
            Box::new(rich),
        )],
    );
    notedeck.set_app(chrome);

    let state = RichState {
        notedeck,
        set_up: false,
        _dir: dir,
    };
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(900.0, 640.0))
        .with_theme(egui::Theme::Dark)
        .renderer(notedeck::software_renderer())
        .build_ui_state(rich_frame, state);
    let _ = harness.run_ok();
    let open = bar_key(&harness.ctx);
    harness.key_press_modifiers(open.modifiers, open.logical_key);
    let _ = harness.run_ok();
    harness.event(Event::Text("rich".to_owned()));
    let _ = harness.run_ok();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !rich_pfp_loaded(&mut harness) {
        assert!(
            std::time::Instant::now() < deadline,
            "the profile's picture never loaded from the disk cache"
        );
        harness.step();
        std::thread::yield_now();
    }
    let _ = harness.run_ok();
    harness.snapshot("command_bar_rich_rows");
}
