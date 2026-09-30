use super::super::file_update::{DiffLine, DiffTag, FileUpdate, FileUpdateType};
use egui::{RichText, Ui};
use notedeck_ui::diff::{self as ui_diff, DiffLines, DiffNumbering, LINE_NUMBER_COLOR};

const EXPAND_LINES_PER_CLICK: usize = 3;

/// Render a file update diff view.
///
/// When `is_local` is true and the update is an Edit, expand-context
/// buttons are shown at the top and bottom of the diff.
pub fn file_update_ui(update: &FileUpdate, is_local: bool, ui: &mut Ui) {
    let can_expand = is_local && matches!(update.update_type, FileUpdateType::Edit { .. });

    // egui temp state for how many extra lines above/below
    let expand_id = ui.scope_id().with("diff_expand").with(&update.file_path);
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

                    // Combined lines: above + core diff + below
                    let combined = ctx
                        .above
                        .iter()
                        .chain(update.diff_lines())
                        .chain(ctx.below.iter());

                    render_diff_lines(
                        combined,
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
                    render_diff_lines(
                        update.diff_lines().iter(),
                        &update.update_type,
                        1,
                        &update.file_path,
                        ui,
                    );
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

/// Render the diff lines with syntax highlighting, numbered sequentially from
/// `start_line` (the 1-based file line number of the first displayed line).
fn render_diff_lines<'a>(
    lines: impl Iterator<Item = &'a DiffLine>,
    update_type: &FileUpdateType,
    start_line: usize,
    file_path: &str,
    ui: &mut Ui,
) {
    let mut numbering = DiffNumbering::starting_at(start_line, start_line);
    // Line numbers only make sense for edits, not whole-file writes.
    let gutter = matches!(update_type, FileUpdateType::Edit { .. });

    DiffLines::for_path(file_path)
        .gutter(gutter)
        .show(lines.map(|l| numbering.row(ui_tag(l.tag), &l.content)), ui);
}

/// Map agentium-core's diff tag onto the renderer's.
fn ui_tag(tag: DiffTag) -> ui_diff::DiffTag {
    match tag {
        DiffTag::Equal => ui_diff::DiffTag::Equal,
        DiffTag::Delete => ui_diff::DiffTag::Delete,
        DiffTag::Insert => ui_diff::DiffTag::Insert,
    }
}

/// Render the file path header (call within a horizontal layout)
pub fn file_path_header(update: &FileUpdate, ui: &mut Ui) {
    let type_label = match &update.update_type {
        FileUpdateType::Edit { .. } => "Edit",
        FileUpdateType::Write { .. } => "Write",
        FileUpdateType::UnifiedDiff { .. } => "Diff",
    };

    ui.label(RichText::new(type_label).strong());
    ui.label(RichText::new(&update.file_path).monospace());
}
