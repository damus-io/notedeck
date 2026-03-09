use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck_ui::context_menu::{stationary_arbitrary_menu_button_padding, MenuPadding};
use notedeck_ui::widgets::search_input_box;

/// Check if a wgpu adapter is available for snapshot rendering.
/// Returns false on headless CI environments without GPU support.
fn wgpu_adapter_available() -> bool {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all() - wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).is_some()
}

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

macro_rules! skip_if_no_gpu {
    () => {
        if !wgpu_adapter_available() {
            eprintln!("Skipping snapshot test: no GPU adapter available");
            return;
        }
    };
}

#[test]
fn test_context_menu_snapshot() {
    skip_if_no_gpu!();

    let mut harness = context_menu_harness(MenuPadding::default());

    let btn = harness.get_by_label("...");
    btn.click();
    harness.run();
    harness.run();

    harness.snapshot("context_menu");
}

#[test]
fn test_context_menu_thin_snapshot() {
    skip_if_no_gpu!();

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

    harness.snapshot("context_menu_thin");
}
