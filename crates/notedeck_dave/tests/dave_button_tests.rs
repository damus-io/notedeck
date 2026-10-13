use egui_kittest::Harness;
use notedeck_dave::ui::InputboxLayout;

/// Renders the Dave inputbox using the real InputboxLayout, with a turn
/// running when `show_stop` (the IsWorking state).
fn dave_inputbox_harness(input: &str, show_stop: bool) -> Harness<'static> {
    let mut text = input.to_string();
    Harness::builder()
        .with_size(egui::Vec2::new(300.0, 120.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            InputboxLayout::new_default(&mut text)
                .show_stop(show_stop)
                .show(ui);
        })
}

/// The buttons the inputbox draws for `input` with a turn running or not,
/// by label: Send is labelled "Ask".
fn inputbox_buttons(input: &str, show_stop: bool) -> [bool; 2] {
    use egui_kittest::kittest::Queryable;
    let mut text = input.to_string();
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(300.0, 120.0))
        .build_ui(move |ui| {
            InputboxLayout::new_default(&mut text)
                .show_stop(show_stop)
                .show(ui);
        });
    harness.run();
    ["Ask", "Stop"].map(|label| harness.query_by_label(label).is_some())
}

/// While a turn runs, Stop takes Send's place, and Send comes back beside
/// it once something is typed to queue.
#[test]
fn stop_takes_sends_place_until_there_is_a_draft() {
    assert_eq!(
        inputbox_buttons("", false),
        [true, false],
        "idle: Send alone"
    );
    assert_eq!(
        inputbox_buttons("", true),
        [false, true],
        "working: Stop alone"
    );
    assert_eq!(
        inputbox_buttons("queue this", true),
        [true, true],
        "working with a draft: both"
    );
    assert_eq!(
        inputbox_buttons("  \n", true),
        [false, true],
        "whitespace is not a draft"
    );
}

/// A turn running on an empty draft: Stop alone, where Send sits.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn test_dave_inputbox_buttons_snapshot() {
    let mut harness = dave_inputbox_harness("", true);
    harness.run();
    harness.snapshot("dave_inputbox_buttons");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn test_dave_inputbox_with_text_snapshot() {
    let mut harness = dave_inputbox_harness(
        "Can you refactor the authentication module\nto use the new token system?",
        false,
    );
    harness.run();
    harness.snapshot("dave_inputbox_with_text");
}
