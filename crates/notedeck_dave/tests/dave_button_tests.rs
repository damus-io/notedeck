use egui::{Align, Layout};
use egui_kittest::Harness;

/// Renders the Dave inputbox buttons with the same layout as the real UI
fn dave_inputbox_buttons_harness() -> Harness<'static> {
    Harness::new_ui(|ui| {
        let base_height = 44.0;
        let line_height = 20.0;
        let input_height = base_height + line_height;
        ui.allocate_ui(egui::vec2(300.0, input_height), |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(egui::Button::new("Ask").min_size(egui::vec2(60.0, 44.0)));

                    ui.add(egui::Button::new("Stop").min_size(egui::vec2(60.0, 44.0)));

                    ui.add(
                        egui::TextEdit::multiline(&mut String::new())
                            .desired_width(f32::INFINITY)
                            .hint_text(egui::RichText::new("Ask dave anything...").weak())
                            .frame(false),
                    );
                });
            });
        });
    })
}

#[test]
fn test_dave_inputbox_buttons_snapshot() {
    let mut harness = dave_inputbox_buttons_harness();
    harness.run();
    harness.snapshot("dave_inputbox_buttons");
}
