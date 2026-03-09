use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck_ui::context_menu::{stationary_arbitrary_menu_button_padding, MenuPadding};
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

fn menu_items(ui: &mut egui::Ui) {
    ui.set_max_width(200.0);
    ui.button("Summarize Thread");
    ui.button("Copy Note Link");
    ui.button("Copy Text");
    ui.button("Copy Pubkey");
    ui.button("Copy Note ID");
    ui.button("Mute User");
}

fn context_menu_harness(padding: MenuPadding) -> Harness<'static> {
    Harness::new_ui(move |ui| {
        let resp = ui.button("...");
        stationary_arbitrary_menu_button_padding(ui, resp, padding, menu_items);
    })
}

fn try_snapshot(harness: &mut Harness<'_>, name: &str) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        harness.snapshot(name);
    }));
    if let Err(e) = result {
        let msg = e
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| e.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        if msg.contains("NoSuitableAdapterFound") || msg.contains("No adapter found") {
            eprintln!("Skipping snapshot '{name}': no GPU adapter available");
        } else {
            std::panic::resume_unwind(e);
        }
    }
}

#[test]
fn test_context_menu_snapshot() {
    let mut harness = context_menu_harness(MenuPadding::default());

    let btn = harness.get_by_label("...");
    btn.click();
    harness.run();
    harness.run();

    try_snapshot(&mut harness, "context_menu");
}

#[test]
fn test_context_menu_thin_snapshot() {
    // egui defaults for comparison: button_padding (4, 1), item_spacing.y = 3
    let thin = MenuPadding {
        button_padding: egui::vec2(4.0, 1.0),
        item_spacing_y: 3.0,
    };
    let mut harness = context_menu_harness(thin);

    let btn = harness.get_by_label("...");
    btn.click();
    harness.run();
    harness.run();

    try_snapshot(&mut harness, "context_menu_thin");
}
