use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck_ui::widgets::search_input_box;

#[test]
fn test_search_input_box_renders() {
    let mut harness = Harness::new_ui_state(
        |ui, query: &mut String| {
            ui.add(search_input_box(query, "Search..."));
        },
        String::new(),
    );

    harness.run();

    // Verify the search input renders with the correct role
    let input = harness.get_by_role(Role::TextInput);
    assert_eq!(input.role(), Role::TextInput);
}

#[test]
fn test_search_input_box_type_text() {
    let mut harness = Harness::new_ui_state(
        |ui, query: &mut String| {
            ui.add(search_input_box(query, "Search..."));
        },
        String::new(),
    );

    harness.run();

    // Click to focus the search input
    let input = harness.get_by_role(Role::TextInput);
    input.click();
    harness.run();

    // Type into the search box
    let input = harness.get_by_role(Role::TextInput);
    input.type_text("hello");
    harness.run();

    // Verify query state was updated
    assert_eq!(harness.state(), "hello");
}
