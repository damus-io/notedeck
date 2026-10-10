//! The Subagents card: every subagent the session spawned, foreground and
//! background, newest first, each with its status, tool uses, tokens and
//! time. A click on a row goes back to the chat with the block cursor on
//! that subagent.
//!
//! Which subagents exist and how they ended comes from the chat's
//! [`Message::Subagent`] rows; their numbers come from the CLI's
//! `task_progress`, folded by [`SubagentState`]. The strings are built when
//! either changes, never per frame.

use crate::inspector::{subagent, ProgressById, SubagentRow, SubagentRows, SubagentState};
use crate::messages::{SubagentInfo, SubagentStatus};
use crate::ui::badge::{BadgeVariant, StatusBadge};
use crate::ui::dave::format_tokens;
use crate::ui::DaveAction;
use crate::Message;
use notedeck::{tr, tr_plural, Localization};

use super::background::short_duration;
use super::{weak_ui, MONO_SIZE, VALUE_SIZE};

/// Diameter of a row's status dot.
const DOT: f32 = 7.0;

/// The Subagents card's body. A click on a row asks to show that subagent in
/// the chat.
pub fn subagents_ui(
    state: &mut SubagentState,
    chat: &[Message],
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let rows = state.rows(chat, i18n, build_rows);
    if rows.rows.is_empty() {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No subagents yet",
                "Dave session inspector subagents: the session has spawned no subagents"
            ),
        );
        return None;
    }

    weak_ui(ui, &rows.summary);
    let mut action = None;
    for row in &rows.rows {
        if row_ui(row, ui).clicked() {
            action = Some(DaveAction::ShowSubagent(row.task_id.clone()));
        }
    }
    action
}

/// One subagent: its type badge, description, and status over the meta
/// line. The whole row is the click target.
fn row_ui(row: &SubagentRow, ui: &mut egui::Ui) -> egui::Response {
    let variant = status_variant(row.status);
    let frame = egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                StatusBadge::new(&row.badge).variant(variant).show(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    weak_ui(ui, &row.status_label);
                    status_dot_ui(ui, variant.colors(ui).fg);
                    // The description takes what the status leaves.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&row.description)
                                    .font(egui::FontId::proportional(VALUE_SIZE)),
                            )
                            .truncate(),
                        );
                    });
                });
            });
            if !row.meta.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&row.meta)
                            .font(egui::FontId::monospace(MONO_SIZE))
                            .weak(),
                    )
                    .truncate(),
                );
            }
        });

    let resp = frame
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect,
            notedeck::tokens::RADIUS_SM,
            ui.visuals().widgets.hovered.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    resp
}

/// A filled circle in `color`, vertically centred in the row.
fn status_dot_ui(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(DOT, DOT), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), DOT / 2.0, color);
}

/// The badge and dot colour for a status, as the chat's subagent row has it.
fn status_variant(status: SubagentStatus) -> BadgeVariant {
    match status {
        SubagentStatus::Running => BadgeVariant::Warning,
        SubagentStatus::Completed => BadgeVariant::Success,
        SubagentStatus::Failed => BadgeVariant::Destructive,
    }
}

/// The card's strings: a summary line and a row per subagent, newest first.
fn build_rows(chat: &[Message], progress: &ProgressById, i18n: &mut Localization) -> SubagentRows {
    let rows: Vec<SubagentRow> = chat
        .iter()
        .rev()
        .filter_map(subagent)
        .map(|info| build_row(info, progress, i18n))
        .collect();
    let summary = summary(&rows, progress, i18n);
    SubagentRows { summary, rows }
}

/// "3 subagents · 1 running · 1 failed · 182K tokens".
fn summary(rows: &[SubagentRow], progress: &ProgressById, i18n: &mut Localization) -> String {
    let count = |status| rows.iter().filter(|r| r.status == status).count();
    let running = count(SubagentStatus::Running);
    let failed = count(SubagentStatus::Failed);
    let tokens: u64 = rows
        .iter()
        .filter_map(|r| progress.get(&r.task_id))
        .map(|p| p.usage.total_tokens)
        .sum();

    let mut parts = vec![tr_plural!(
        i18n,
        "{count} subagent",
        "{count} subagents",
        "Dave session inspector subagents: how many subagents the session spawned",
        rows.len() as i64,
    )];
    if running > 0 {
        parts.push(tr!(
            i18n,
            "{count} running",
            "Dave session inspector subagents: how many subagents are still running",
            count = running
        ));
    }
    if failed > 0 {
        parts.push(tr!(
            i18n,
            "{count} failed",
            "Dave session inspector subagents: how many subagents failed",
            count = failed
        ));
    }
    if tokens > 0 {
        let tokens = format_tokens(tokens);
        parts.push(tr!(
            i18n,
            "{tokens} tokens",
            "Dave session inspector subagents: tokens all the session's subagents used",
            tokens = tokens.as_str()
        ));
    }
    parts.join(" · ")
}

/// One subagent's strings, with its progress if it reported any.
fn build_row(info: &SubagentInfo, progress: &ProgressById, i18n: &mut Localization) -> SubagentRow {
    let status_label = match info.status {
        SubagentStatus::Running if info.background => tr!(
            i18n,
            "in background",
            "Dave session inspector subagents: a background subagent still running"
        ),
        SubagentStatus::Running => tr!(
            i18n,
            "running",
            "Dave session inspector subagents: a foreground subagent still running"
        ),
        SubagentStatus::Completed => tr!(
            i18n,
            "done",
            "Dave session inspector subagents: a subagent that finished"
        ),
        SubagentStatus::Failed => tr!(
            i18n,
            "failed",
            "Dave session inspector subagents: a subagent that failed"
        ),
    };

    let mut meta = String::new();
    if let Some(progress) = progress.get(&info.task_id) {
        let usage = progress.usage;
        let mut part = |text: &str| {
            if !meta.is_empty() {
                meta.push_str(" · ");
            }
            meta.push_str(text);
        };
        part(&tr_plural!(
            i18n,
            "{count} tool use",
            "{count} tool uses",
            "Dave session inspector subagents: tool calls a subagent has made",
            usage.tool_uses as i64,
        ));
        let tokens = format_tokens(usage.total_tokens);
        part(&tr!(
            i18n,
            "{tokens} tokens",
            "Dave session inspector subagents: tokens a subagent has used",
            tokens = tokens.as_str()
        ));
        if usage.duration_ms > 0 {
            part(&short_duration(usage.duration_ms / 1000, true));
        }
        if let Some(tool) = &progress.last_tool_name {
            part(&tr!(
                i18n,
                "last {tool}",
                "Dave session inspector subagents: the tool a subagent called last",
                tool = tool.as_str()
            ));
        }
    }

    SubagentRow {
        task_id: info.task_id.clone(),
        badge: info.subagent_type.clone(),
        description: info.description.clone(),
        status: info.status,
        status_label,
        meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{DaveApiResponse, SubagentProgress, TaskUsage};

    fn spawned(task_id: &str, kind: &str, description: &str, background: bool) -> Message {
        Message::Subagent(SubagentInfo {
            task_id: task_id.into(),
            description: description.into(),
            subagent_type: kind.into(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background,
        })
    }

    fn set_status(chat: &mut [Message], task_id: &str, status: SubagentStatus) {
        for message in chat {
            if let Message::Subagent(info) = message {
                if info.task_id == task_id {
                    info.status = status;
                }
            }
        }
    }

    /// Spawns, a completion, a failure and progress reports become one row
    /// each, newest first, with their counts on the meta line and in the
    /// summary.
    #[test]
    fn rows_carry_status_and_progress() {
        let mut i18n = Localization::default();
        let mut inspector = crate::inspector::SessionInspector::default();
        let mut chat = vec![
            spawned("t1", "Explore", "find the parser", false),
            spawned("t2", "general-purpose", "write the tests", false),
            spawned("t3", "Plan", "survey CLI schemas", true),
        ];
        set_status(&mut chat, "t1", SubagentStatus::Completed);
        set_status(&mut chat, "t2", SubagentStatus::Failed);
        for (task_id, tokens, tools, last) in [
            ("t1", 12_400, 7, Some("Grep")),
            ("t3", 3_000, 2, Some("Read")),
            ("t3", 5_000, 3, None),
        ] {
            inspector.apply(&DaveApiResponse::SubagentProgress(SubagentProgress {
                task_id: task_id.into(),
                usage: TaskUsage {
                    total_tokens: tokens,
                    tool_uses: tools,
                    duration_ms: 61_000,
                },
                last_tool_name: last.map(str::to_string),
            }));
        }

        let rows = inspector.subagents.rows(&chat, &mut i18n, build_rows);
        let ids: Vec<&str> = rows.rows.iter().map(|r| r.task_id.as_str()).collect();
        assert_eq!(ids, ["t3", "t2", "t1"], "newest first");

        let [t3, t2, t1] = &rows.rows[..] else {
            panic!("three rows");
        };
        assert_eq!(t3.badge, "Plan");
        assert_eq!(t3.status_label, "in background");
        assert!(t3.meta.contains("3"), "{:?}", t3.meta);
        assert!(t3.meta.contains("5K"), "{:?}", t3.meta);
        assert!(t3.meta.contains("Read"), "kept from the earlier report");
        assert_eq!(t2.status_label, "failed");
        assert!(t2.meta.is_empty(), "no progress, no meta");
        assert_eq!(t1.status_label, "done");
        for part in ["7", "12K", "1m 1s", "Grep"] {
            assert!(t1.meta.contains(part), "{part} in {:?}", t1.meta);
        }
        for part in ["3", "1", "17K"] {
            assert!(rows.summary.contains(part), "{part} in {:?}", rows.summary);
        }
    }

    /// Rows are built once per change, not once per frame.
    #[test]
    fn rows_rebuild_only_when_a_subagent_changes() {
        let mut i18n = Localization::default();
        let mut state = SubagentState::default();
        let mut chat = vec![spawned("t1", "Explore", "find it", false)];
        let mut builds = 0;
        let mut build = |chat: &[Message], progress: &ProgressById, i18n: &mut Localization| {
            builds += 1;
            build_rows(chat, progress, i18n)
        };
        state.rows(&chat, &mut i18n, &mut build);
        state.rows(&chat, &mut i18n, &mut build);
        set_status(&mut chat, "t1", SubagentStatus::Completed);
        state.rows(&chat, &mut i18n, &mut build);
        assert_eq!(builds, 2);
    }
}
