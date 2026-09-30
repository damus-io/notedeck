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

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_dashboard_icon() {
    let mut h = icon_harness(|ui| {
        icons::dashboard_icon(ui, 24.0);
    });
    h.run();
    h.snapshot("dashboard_icon");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_messages_icon() {
    let mut h = icon_harness(|ui| {
        icons::messages_icon(ui, 24.0);
    });
    h.run();
    h.snapshot("messages_icon");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_notebook_icon() {
    let mut h = icon_harness(|ui| {
        icons::notebook_icon(ui, 24.0);
    });
    h.run();
    h.snapshot("notebook_icon");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_headway_icon() {
    let mut h = icon_harness(|ui| {
        icons::headway_icon(ui, 24.0);
    });
    h.run();
    h.snapshot("headway_icon");
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

// ---------------------------------------------------------------------------
// Git patch widget
// ---------------------------------------------------------------------------

/// Render `patch` at `width` in notedeck's fonts, under a parent with no
/// horizontal item gap, as the chrome gives every app, and snapshot it as
/// `name`.
fn git_patch_snapshot(name: &str, patch: &str, width: f32) {
    use notedeck_ui::diff::{git_patch_ui, GitPatch, GitPatchState};

    let patch = GitPatch::parse(patch);
    let state = GitPatchState::new(&patch, &mut notedeck::Localization::default());
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(width, 900.0))
        .renderer(notedeck::software_renderer())
        .build_ui_state(
            |ui, (patch, state): &mut (GitPatch, GitPatchState)| {
                ui.spacing_mut().item_spacing.x = 0.0;
                git_patch_ui(patch, state, ui)
            },
            (patch, state),
        );
    notedeck::fonts::setup_fonts(&harness.ctx);
    harness.run();
    harness.snapshot(name);
}

/// The multi-file patch view: summary, file headers, hunk headers and
/// syntax-highlighted diff rows with their gutters.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_git_patch() {
    git_patch_snapshot(
        "git_patch",
        include_str!("../src/diff/testdata/multi.patch"),
        640.0,
    );
}

/// The patch view too narrow for its paths: each path keeps its file name and
/// gives up the end of its directory, and the stats stay in their columns.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_git_patch_narrow() {
    git_patch_snapshot(
        "git_patch_narrow",
        include_str!("../src/diff/testdata/nested.patch"),
        340.0,
    );
}

/// Prose ink in the baseline tests is pure blue; monospace ink is red or
/// amber. The bottom row of either set of `H`s is its baseline (no descenders).
const PROSE_INK: egui::Color32 = egui::Color32::from_rgb(0, 0, 255);

/// The bottom rows of the prose and the monospace ink in `img`, told apart by
/// colour: prose is blue with no red, monospace carries red.
fn prose_and_mono_baselines(img: &image::RgbaImage) -> (u32, u32) {
    let (mut prose, mut mono) = (None, None);
    for (_, y, p) in img.enumerate_pixels() {
        let [r, _, b, _] = p.0;
        if r < 20 && b > 100 {
            prose = Some(y);
        } else if r > 80 {
            mono = Some(y);
        }
    }
    (
        prose.expect("no prose ink rendered"),
        mono.expect("no monospace ink rendered"),
    )
}

/// Render `ui_fn` with notedeck's fonts on a black panel at body `size`, and
/// return how far the monospace baseline sits below the prose one, in pixels.
fn mono_baseline_offset(size: f32, ui_fn: impl Fn(&mut egui::Ui) + 'static) -> i64 {
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(320.0, 60.0))
        .renderer(notedeck::software_renderer())
        .build(move |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    ui.style_mut().visuals.override_text_color = Some(PROSE_INK);
                    ui.style_mut()
                        .text_styles
                        .insert(egui::TextStyle::Body, egui::FontId::proportional(size));
                    ui_fn(ui);
                });
        });
    notedeck::fonts::setup_fonts(&harness.ctx);
    harness.run();
    let img = harness.render().expect("render");
    let (prose, mono) = prose_and_mono_baselines(&img);
    mono as i64 - prose as i64
}

/// Sizes the baseline tests check: small labels, the desktop body and the
/// mobile/note body.
const BASELINE_SIZES: [f32; 3] = [11.0, 13.0, 16.0];

/// Inline code in markdown prose sits on the prose baseline.
///
/// Both runs share one `LayoutJob` row. Inconsolata's row is shorter than
/// Onest's, so this pins the pair of settings that keep them level: the
/// Inconsolata `y_offset_factor` in `notedeck::fonts` and the inline-code
/// `line_height` in `notedeck_ui::markdown`. The -0.18 factor that was there first
/// and the 0.0 that replaced it measured 2-3px off here or in the centred row
/// below.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn monospace_inline_code_sits_on_the_prose_baseline() {
    for size in BASELINE_SIZES {
        let offset = mono_baseline_offset(size, |ui| {
            notedeck_ui::markdown::render_markdown("HHHH `HHHH`", ui);
        });
        assert!(
            offset.abs() <= 1,
            "{size}pt inline code baseline is {offset}px off the prose baseline"
        );
    }
}

/// A monospace label beside a proportional one in a centred row (a sha pill, a
/// session chip, a path after a verb) shares its baseline.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn monospace_label_sits_on_the_baseline_of_a_centred_row() {
    for size in BASELINE_SIZES {
        let offset = mono_baseline_offset(size, move |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("HHHH").size(size));
                ui.label(
                    egui::RichText::new("HHHH")
                        .monospace()
                        .size(size)
                        .color(egui::Color32::RED),
                );
            });
        });
        assert!(
            offset.abs() <= 1,
            "{size}pt monospace label baseline is {offset}px off its proportional neighbour"
        );
    }
}
