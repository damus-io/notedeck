//! Pixel baselines for Dave's file-update diff view.
//!
//! The line renderer behind [`file_update_ui`] lives in `notedeck_ui::diff`
//! and is shared with headway's git patch widget, so a change made there for
//! headway can shift Dave's edit view without touching this crate. These
//! snapshots pin both shapes Dave shows: an Edit (numbered gutter, `-`/`+`
//! rows, syntax highlighting) and a Write (no gutter, every row `+`).
//!
//! The harness uses egui's default visuals, not notedeck's `ColorTheme`, so the
//! PNGs are a regression baseline rather than a reference for colour contrast.

use egui_kittest::Harness;
use notedeck_dave::file_update::{FileUpdate, FileUpdateType};
use notedeck_dave::ui::diff::{file_path_header, file_update_ui};

/// Harness rendering `update` the way Dave's tool view does: the path header
/// row above the diff body.
///
/// `is_local` is false so the expand-context buttons never read the (absent)
/// file from disk.
fn diff_harness(update: FileUpdate, height: f32) -> Harness<'static> {
    Harness::builder()
        .with_size(egui::Vec2::new(520.0, height))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            ui.horizontal(|ui| file_path_header(&update, ui));
            file_update_ui(&update, false, ui);
        })
}

/// An Edit to a Rust file: context, a removed line and two added lines.
fn rust_edit() -> FileUpdate {
    FileUpdate::new(
        "src/greeting.rs".to_owned(),
        FileUpdateType::Edit {
            old_string: "\
fn greet(name: &str) -> String {
    let greeting = \"Hello\";
    format!(\"{greeting}, {name}!\")
}"
            .to_owned(),
            new_string: "\
fn greet(name: &str) -> String {
    // a friendlier default
    let greeting = \"Hi there\";
    format!(\"{greeting}, {name}!\")
}"
            .to_owned(),
        },
    )
}

/// A Write creating a small TOML file.
fn toml_write() -> FileUpdate {
    FileUpdate::new(
        "config/settings.toml".to_owned(),
        FileUpdateType::Write {
            content: "\
[server]
host = \"127.0.0.1\"
port = 8080
"
            .to_owned(),
        },
    )
}

/// The fixtures diff as intended, so a snapshot mismatch points at the
/// renderer rather than at a fixture that no longer produces a diff.
#[test]
fn fixtures_produce_expected_rows() {
    use notedeck_dave::file_update::DiffTag;

    let tags = |u: &FileUpdate| u.diff_lines().iter().map(|l| l.tag).collect::<Vec<_>>();

    let edit = tags(&rust_edit());
    assert_eq!(edit.iter().filter(|t| **t == DiffTag::Delete).count(), 1);
    assert_eq!(edit.iter().filter(|t| **t == DiffTag::Insert).count(), 2);
    assert!(edit.contains(&DiffTag::Equal));

    let write = tags(&toml_write());
    assert_eq!(write, vec![DiffTag::Insert; 3]);
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_file_update_edit() {
    let mut harness = diff_harness(rust_edit(), 200.0);
    harness.run();
    harness.snapshot("file_update_edit");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_file_update_write() {
    let mut harness = diff_harness(toml_write(), 130.0);
    harness.run();
    harness.snapshot("file_update_write");
}
