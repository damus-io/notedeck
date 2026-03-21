use egui_kittest::Harness;
use notedeck_dave::ui::InputboxLayout;

/// Renders the Dave inputbox using the real InputboxLayout with both
/// Ask and Stop buttons visible (simulates IsWorking + !IsRemote state).
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
