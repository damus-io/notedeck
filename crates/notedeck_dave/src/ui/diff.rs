use super::super::file_update::{DiffLine, DiffTag, FileUpdate, FileUpdateType};
use super::markdown_ui::{tokenize_code, SandCodeTheme};
use egui::text::LayoutJob;
use egui::{Color32, FontId, RichText, TextFormat, Ui};

/// Colors for diff rendering
const DELETE_COLOR: Color32 = Color32::from_rgb(200, 60, 60);
const INSERT_COLOR: Color32 = Color32::from_rgb(60, 180, 60);
const LINE_NUMBER_COLOR: Color32 = Color32::from_rgb(128, 128, 128);
const EXPAND_LINES_PER_CLICK: usize = 3;

/// Soft background tints for syntax-highlighted diff lines.
/// Uses premultiplied alpha: rgb(200,60,60) @ alpha=40 and rgb(60,180,60) @ alpha=40.
const DELETE_BG: Color32 = Color32::from_rgba_premultiplied(31, 9, 9, 40);
const INSERT_BG: Color32 = Color32::from_rgba_premultiplied(9, 28, 9, 40);

/// Render a file update diff view.
///
/// When `is_local` is true and the update is an Edit, expand-context
/// buttons are shown at the top and bottom of the diff.
pub fn file_update_ui(update: &FileUpdate, is_local: bool, ui: &mut Ui) {
    let can_expand = is_local && matches!(update.update_type, FileUpdateType::Edit { .. });

    // egui temp state for how many extra lines above/below
    let expand_id = ui.id().with("diff_expand").with(&update.file_path);
    let (extra_above, extra_below): (usize, usize) = if can_expand {
        ui.data(|d| d.get_temp(expand_id).unwrap_or((0, 0)))
    } else {
        (0, 0)
    };

    // Try to compute expanded context from the file on disk
    let expanded = if can_expand {
        update.expanded_context(extra_above, extra_below)
    } else {
        None
    };

    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                if let Some(ctx) = &expanded {
                    // "Expand above" button
                    if ctx.has_more_above && expand_button(ui, true) {
                        ui.data_mut(|d| {
                            d.insert_temp(
                                expand_id,
                                (extra_above + EXPAND_LINES_PER_CLICK, extra_below),
                            );
                        });
                    }

                    // Build combined lines: above + core diff + below
                    let combined: Vec<&DiffLine> = ctx
                        .above
                        .iter()
                        .chain(update.diff_lines().iter())
                        .chain(ctx.below.iter())
                        .collect();

                    render_diff_lines(
                        &combined,
                        &update.update_type,
                        ctx.start_line,
                        &update.file_path,
                        ui,
                    );

                    // "Expand below" button
                    if ctx.has_more_below && expand_button(ui, false) {
                        ui.data_mut(|d| {
                            d.insert_temp(
                                expand_id,
                                (extra_above, extra_below + EXPAND_LINES_PER_CLICK),
                            );
                        });
                    }
                } else {
                    // No expansion available: render as before (line numbers from 1)
                    let refs: Vec<&DiffLine> = update.diff_lines().iter().collect();
                    render_diff_lines(&refs, &update.update_type, 1, &update.file_path, ui);
                }
            });
        });
}

/// Render a clickable expand-context button. Returns true if clicked.
fn expand_button(ui: &mut Ui, is_above: bool) -> bool {
    let text = if is_above {
        "  \u{25B2} Show more context above"
    } else {
        "  \u{25BC} Show more context below"
    };
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .monospace()
                .size(11.0)
                .color(LINE_NUMBER_COLOR),
        )
        .sense(egui::Sense::click()),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
}

/// Render the diff lines with syntax highlighting.
///
/// `start_line` is the 1-based file line number of the first displayed line.
fn render_diff_lines(
    lines: &[&DiffLine],
    update_type: &FileUpdateType,
    start_line: usize,
    file_path: &str,
    ui: &mut Ui,
) {
    let mut old_line = start_line;
    let mut new_line = start_line;

    let font_id = FontId::new(12.0, egui::FontFamily::Monospace);
    let theme = SandCodeTheme::from_visuals(ui.visuals());
    let lang = file_extension(file_path).unwrap_or("text");

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

            // Prefix character and its strong diff color
            let (prefix, prefix_color) = match diff_line.tag {
                DiffTag::Equal => (" ", ui.visuals().text_color()),
                DiffTag::Delete => ("-", DELETE_COLOR),
                DiffTag::Insert => ("+", INSERT_COLOR),
            };

            // Remove trailing newline for display
            let content = diff_line.content.trim_end_matches('\n');

            // Background tint signals diff status
            let line_bg = match diff_line.tag {
                DiffTag::Equal => Color32::TRANSPARENT,
                DiffTag::Delete => DELETE_BG,
                DiffTag::Insert => INSERT_BG,
            };

            let mut job = LayoutJob::default();

            // Prefix with diff color
            job.append(
                &format!("{} ", prefix),
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: prefix_color,
                    background: line_bg,
                    ..Default::default()
                },
            );

            // Syntax-highlighted content
            for (token, text) in tokenize_code(content, lang) {
                let mut fmt = theme.format(token, &font_id);
                fmt.background = line_bg;
                job.append(text, 0.0, fmt);
            }

            ui.label(job);
        });
    }
}

/// Extract file extension from a path.
fn file_extension(path: &str) -> Option<&str> {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
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
