use super::super::file_update::{DiffLine, DiffTag, FileUpdate, FileUpdateType};
use egui::{Color32, RichText, Ui};

/// Colors for diff rendering
const DELETE_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const INSERT_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const LINE_NUMBER_COLOR: Color32 = Color32::from_rgb(128, 128, 128);

/// Render a file update diff view
pub fn file_update_ui(update: &FileUpdate, ui: &mut Ui) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            let max_height = ui.available_height().min(400.0);
            egui::ScrollArea::both()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    render_diff_lines(update.diff_lines(), &update.update_type, ui);
                });
        });
}

/// Render the diff lines with proper coloring
fn render_diff_lines(lines: &[DiffLine], update_type: &FileUpdateType, ui: &mut Ui) {
    // Track line numbers for old and new
    let mut old_line = 1usize;
    let mut new_line = 1usize;

    for diff_line in lines {
        ui.horizontal(|ui| {
            // Line number gutter
            let (old_num, new_num) = match diff_line.tag {
                DiffTag::Equal => {
                    let result = (Some(old_line), Some(new_line));
                    old_line += 1;
                    new_line += 1;
                    result
                }
                DiffTag::Delete => {
                    let result = (Some(old_line), None);
                    old_line += 1;
                    result
                }
                DiffTag::Insert => {
                    let result = (None, Some(new_line));
                    new_line += 1;
                    result
                }
            };

            // Render line numbers (only for edits, not writes)
            if matches!(update_type, FileUpdateType::Edit { .. }) {
                let old_str = old_num
                    .map(|n| format!("{:4}", n))
                    .unwrap_or_else(|| "    ".to_string());
                let new_str = new_num
                    .map(|n| format!("{:4}", n))
                    .unwrap_or_else(|| "    ".to_string());

                ui.label(
                    RichText::new(format!("{} {}", old_str, new_str))
                        .monospace()
                        .size(11.0)
                        .color(LINE_NUMBER_COLOR),
                );
            }

            // Render the prefix and content
            let (prefix, color) = match diff_line.tag {
                DiffTag::Equal => (" ", ui.visuals().text_color()),
                DiffTag::Delete => ("-", DELETE_COLOR),
                DiffTag::Insert => ("+", INSERT_COLOR),
            };

            // Remove trailing newline for display
            let content = diff_line.content.trim_end_matches('\n');

            ui.label(
                RichText::new(format!("{} {}", prefix, content))
                    .monospace()
                    .size(12.0)
                    .color(color),
            );
        });
    }
}

/// Render the file path header (call within a horizontal layout)
pub fn file_path_header(update: &FileUpdate, ui: &mut Ui) {
    let type_label = match &update.update_type {
        FileUpdateType::Edit { .. } => "Edit",
        FileUpdateType::Write { .. } => "Write",
    };

    ui.label(RichText::new(type_label).strong());
    ui.label(RichText::new(&update.file_path).monospace());
}
