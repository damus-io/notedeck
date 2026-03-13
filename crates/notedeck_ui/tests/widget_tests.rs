use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck_ui::context_menu::{stationary_arbitrary_menu_button_padding, MenuPadding};
use notedeck_ui::icons;
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
    let _ = ui.button("Summarize Thread");
    let _ = ui.button("Copy Note Link");
    let _ = ui.button("Copy Text");
    let _ = ui.button("Copy Pubkey");
    let _ = ui.button("Copy Note ID");
    let _ = ui.button("Mute User");
}

fn context_menu_harness(padding: MenuPadding) -> Harness<'static> {
    Harness::new_ui(move |ui| {
        let resp = ui.button("...");
        stationary_arbitrary_menu_button_padding(ui, resp, padding, menu_items);
    })
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn test_context_menu_snapshot() {
    let mut harness = context_menu_harness(MenuPadding::default());

    let btn = harness.get_by_label("...");
    btn.click();
    harness.run();
    harness.run();

    harness.snapshot("context_menu");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
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

    harness.snapshot("context_menu_thin");
}

// ---------------------------------------------------------------------------
// Toolbar icon snapshots — painter-drawn, no external assets needed
// ---------------------------------------------------------------------------

fn icon_harness(f: impl Fn(&mut egui::Ui) + 'static) -> Harness<'static> {
    Harness::builder()
        .with_size(egui::Vec2::new(64.0, 64.0))
        .renderer(notedeck::software_renderer())
        .build_ui(f)
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_home_inactive() {
    let mut h = icon_harness(|ui| {
        icons::home_button(ui, 24.0, false);
    });
    h.run();
    h.snapshot("home_inactive");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_home_active() {
    let mut h = icon_harness(|ui| {
        icons::home_button(ui, 24.0, true);
    });
    h.run();
    h.snapshot("home_active");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_chat_inactive() {
    let mut h = icon_harness(|ui| {
        icons::chat_button(ui, 24.0, false);
    });
    h.run();
    h.snapshot("chat_inactive");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_chat_active() {
    let mut h = icon_harness(|ui| {
        icons::chat_button(ui, 24.0, true);
    });
    h.run();
    h.snapshot("chat_active");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notifications_inactive() {
    let mut h = icon_harness(|ui| {
        icons::notifications_button(ui, 24.0, false, false);
    });
    h.run();
    h.snapshot("notifications_inactive");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notifications_active() {
    let mut h = icon_harness(|ui| {
        icons::notifications_button(ui, 24.0, true, false);
    });
    h.run();
    h.snapshot("notifications_active");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notifications_unseen() {
    let mut h = icon_harness(|ui| {
        icons::notifications_button(ui, 24.0, false, true);
    });
    h.run();
    h.snapshot("notifications_unseen");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_search_button_inactive() {
    let mut h = icon_harness(|ui| {
        ui.add(icons::search_button(egui::Color32::WHITE, 1.5, false));
    });
    h.run();
    h.snapshot("search_button_inactive");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_search_button_active() {
    let mut h = icon_harness(|ui| {
        ui.add(icons::search_button(egui::Color32::WHITE, 1.5, true));
    });
    h.run();
    h.snapshot("search_button_active");
}

// ---------------------------------------------------------------------------
// Composite widget snapshots
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_search_input() {
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(300.0, 50.0))
        .renderer(notedeck::software_renderer())
        .build_ui(|ui| {
            let mut query = String::new();
            ui.add(search_input_box(&mut query, "Search..."));
        });
    harness.run();
    harness.snapshot("search_input");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_toolbar_row() {
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(300.0, 64.0))
        .renderer(notedeck::software_renderer())
        .build_ui(|ui| {
            ui.horizontal(|ui| {
                icons::home_button(ui, 24.0, true);
                ui.add(icons::search_button(egui::Color32::WHITE, 1.5, false));
                icons::chat_button(ui, 24.0, false);
                icons::notifications_button(ui, 24.0, false, true);
            });
        });
    harness.run();
    harness.snapshot("toolbar_row");
}

// ---------------------------------------------------------------------------
// AccessKit interaction tests — query icons by label
// ---------------------------------------------------------------------------

#[test]
fn accesskit_home_button_queryable() {
    let harness = Harness::new_ui(|ui| {
        icons::home_button(ui, 24.0, false);
    });
    harness.get_by_label("Home");
}

#[test]
fn accesskit_messages_button_queryable() {
    let harness = Harness::new_ui(|ui| {
        icons::chat_button(ui, 24.0, false);
    });
    harness.get_by_label("Messages");
}

#[test]
fn accesskit_notifications_button_queryable() {
    let harness = Harness::new_ui(|ui| {
        icons::notifications_button(ui, 24.0, false, false);
    });
    harness.get_by_label("Notifications");
}

#[test]
fn accesskit_search_button_queryable() {
    let harness = Harness::new_ui(|ui| {
        ui.add(icons::search_button(egui::Color32::WHITE, 1.5, false));
    });
    harness.get_by_label("Search");
}

#[test]
fn accesskit_toolbar_all_buttons_queryable() {
    let harness = Harness::new_ui(|ui| {
        ui.horizontal(|ui| {
            icons::home_button(ui, 24.0, true);
            ui.add(icons::search_button(egui::Color32::WHITE, 1.5, false));
            icons::chat_button(ui, 24.0, false);
            icons::notifications_button(ui, 24.0, false, false);
        });
    });

    // All four buttons should be findable by their AccessKit labels
    harness.get_by_label("Home");
    harness.get_by_label("Search");
    harness.get_by_label("Messages");
    harness.get_by_label("Notifications");
}
