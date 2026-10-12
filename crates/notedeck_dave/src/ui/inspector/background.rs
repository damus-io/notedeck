//! The Background card: every background task, Monitor, wake-up and cron
//! the session has outstanding, newest first, each with what it is doing
//! rather than the counts the Waiting status shows.
//!
//! The entries come from the Claude backend's `WaitTracker` as
//! [`DaveApiResponse::BackgroundWork`](crate::messages::DaveApiResponse::BackgroundWork).
//! Their strings are built into [`BackgroundRow`]s once a second at most (ages
//! and countdowns move with the clock), and a local task's output tail is
//! read off the UI thread by [`OutputTails`](crate::inspector::OutputTails).
//!
//! A task (not a wake-up or cron, which only the model can cancel) has a
//! Stop control: Stop, then "Stop? Yes No", then "stopping…" until the
//! CLI reports it ended. See [`TaskStops`](crate::inspector::TaskStops).

use crate::inspector::{
    BackgroundParts, BackgroundRow, BackgroundState, LabelMemo, StopLabels, StopStage,
};
use crate::messages::{
    BackgroundEntry, BackgroundTask, BackgroundTaskKind, BackgroundWork, WaitingOn,
};
use crate::ui::dave::format_tokens;
use crate::ui::DaveAction;
use notedeck::{tr, Localization};
use std::time::Instant;

use super::{weak_ui, MONO_SIZE, VALUE_SIZE};

/// Size of the kind badge's text.
const BADGE_SIZE: f32 = 9.5;

/// The Background card's body, as of `now_unix`, and the task a confirmed
/// Stop asked to stop. A remote session shows no output tails, the files
/// being on its host, and no Stop, its tasks being out of reach.
pub fn background_ui(
    state: &mut BackgroundState,
    is_remote: bool,
    now_unix: u64,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let BackgroundParts {
        entries,
        generation,
        rows,
        tails,
        stops,
        stop_labels,
    } = state.parts_mut();
    if entries.is_empty() {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "Nothing running in the background",
                "Dave session inspector background: no background tasks, wake-ups or crons"
            ),
        );
        return None;
    }

    let rows = rows.get((generation, now_unix), i18n, |_, i18n| {
        entries
            .iter()
            .map(|entry| build_row(entry, now_unix, i18n))
            .collect()
    });
    let labels = stop_labels.get((), i18n, |_, i18n| build_stop_labels(i18n));
    let now = Instant::now();
    let mut action = None;
    for (entry, row) in entries.iter().zip(rows) {
        let tail = if is_remote {
            None
        } else {
            entry.output_file().map(|path| tails.tail(path, now))
        };
        let stop = (!is_remote && matches!(entry.work, BackgroundWork::Task(_)))
            .then(|| stops.stage(&entry.id, now));
        let stop = stop.map(|stage| StopControl { stage, labels });
        match entry_ui(entry, row, tail, stop, ui) {
            Some(StopClick::Ask) => stops.ask(&entry.id),
            Some(StopClick::No) => stops.cancel(),
            Some(StopClick::Yes) => {
                stops.confirm(&entry.id, now);
                action = Some(DaveAction::StopTask(entry.id.clone()));
            }
            None => {}
        }
    }

    // Ages and countdowns tick each second, a refused stop is offered again
    // within one, and the tails refresh every few.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));
    action
}

/// A task's Stop control as drawn: where it stands, in which words.
#[derive(Clone, Copy)]
struct StopControl<'a> {
    stage: StopStage,
    labels: &'a StopLabels,
}

/// What a click on a task's Stop control asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopClick {
    /// Stop: ask to confirm.
    Ask,
    /// Yes: stop it.
    Yes,
    /// No: withdraw the question.
    No,
}

/// One entry: the kind badge, title, age and Stop control over the meta
/// line, then the output tail when there is one. Returns a click on Stop.
fn entry_ui(
    entry: &BackgroundEntry,
    row: &BackgroundRow,
    tail: Option<&str>,
    stop: Option<StopControl<'_>>,
    ui: &mut egui::Ui,
) -> Option<StopClick> {
    let mut click = None;
    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                badge_ui(ui, &row.badge, badge_colors(entry, ui.visuals().dark_mode));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(stop) = stop {
                        click = stop_ui(stop, ui);
                    }
                    weak_ui(ui, &row.when);
                    // The title takes what the age leaves, from the left.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let font = if row.title_is_code {
                            egui::FontId::monospace(MONO_SIZE + 1.0)
                        } else {
                            egui::FontId::proportional(VALUE_SIZE)
                        };
                        ui.add(
                            egui::Label::new(egui::RichText::new(&row.title).font(font)).truncate(),
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
            if let Some(tail) = tail.filter(|t| !t.is_empty()) {
                tail_ui(ui, tail);
            }
        });
    click
}

/// The Stop control, laid out right to left: Stop, or "Stop? Yes No", or
/// "stopping…". Returns what was clicked.
fn stop_ui(stop: StopControl<'_>, ui: &mut egui::Ui) -> Option<StopClick> {
    let labels = stop.labels;
    match stop.stage {
        StopStage::Offered => ui
            .small_button(labels.stop.as_str())
            .clicked()
            .then_some(StopClick::Ask),
        StopStage::Confirming => {
            // Right to left: the last-read word goes first.
            let no = ui.small_button(labels.no.as_str()).clicked();
            let yes = ui.small_button(labels.yes.as_str()).clicked();
            ui.label(
                egui::RichText::new(labels.question.as_str())
                    .font(egui::FontId::proportional(VALUE_SIZE)),
            );
            if yes {
                Some(StopClick::Yes)
            } else if no {
                Some(StopClick::No)
            } else {
                None
            }
        }
        StopStage::Stopping => {
            weak_ui(ui, &labels.stopping);
            None
        }
    }
}

/// The Stop control's words, in the current locale.
fn build_stop_labels(i18n: &mut Localization) -> StopLabels {
    StopLabels {
        stop: tr!(
            i18n,
            "Stop",
            "Dave session inspector background: button that stops a background task"
        ),
        question: tr!(
            i18n,
            "Stop?",
            "Dave session inspector background: asks to confirm stopping a background task"
        ),
        yes: tr!(
            i18n,
            "Yes",
            "Dave session inspector background: confirms stopping a background task"
        ),
        no: tr!(
            i18n,
            "No",
            "Dave session inspector background: keeps a background task running"
        ),
        stopping: tr!(
            i18n,
            "stopping…",
            "Dave session inspector background: a stop was sent and the task hasn't ended yet"
        ),
    }
}

/// The kind badge: small uppercase mono text on a tinted chip.
fn badge_ui(ui: &mut egui::Ui, text: &str, colors: BadgeColors) {
    egui::Frame::new()
        .fill(colors.bg)
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .font(egui::FontId::monospace(BADGE_SIZE))
                    .color(colors.fg),
            );
        });
}

/// A task's last lines of output, in mono on a dark block. Long lines wrap,
/// so a log can't widen the card past its column.
pub(super) fn tail_ui(ui: &mut egui::Ui, tail: &str) {
    egui::Frame::new()
        .fill(ui.visuals().code_bg_color)
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tail).font(egui::FontId::monospace(MONO_SIZE)),
                )
                .wrap(),
            );
        });
}

/// A kind badge's text and chip colours.
#[derive(Clone, Copy)]
struct BadgeColors {
    fg: egui::Color32,
    bg: egui::Color32,
}

/// The badge's text and chip colours, one hue per kind. Dark mode puts
/// light text on a dark tint; light mode swaps them.
fn badge_colors(entry: &BackgroundEntry, dark_mode: bool) -> BadgeColors {
    use egui::Color32;
    let (fg, bg) = match &entry.work {
        BackgroundWork::Task(task) => match task.kind {
            BackgroundTaskKind::Bash => (
                Color32::from_rgb(0xA8, 0xD5, 0x9C),
                Color32::from_rgb(0x2E, 0x3A, 0x2A),
            ),
            BackgroundTaskKind::Monitor => (
                Color32::from_rgb(0x9C, 0xC0, 0xE8),
                Color32::from_rgb(0x2B, 0x33, 0x40),
            ),
            BackgroundTaskKind::Agent | BackgroundTaskKind::Workflow => (
                Color32::from_rgb(0xEB, 0xA6, 0xE6),
                Color32::from_rgb(0x3A, 0x26, 0x38),
            ),
            BackgroundTaskKind::Mcp | BackgroundTaskKind::Other => (
                Color32::from_rgb(0xC8, 0xC8, 0xC8),
                Color32::from_rgb(0x38, 0x38, 0x38),
            ),
        },
        BackgroundWork::Wakeup { .. } => (
            Color32::from_rgb(0xCD, 0xBD, 0xF0),
            Color32::from_rgb(0x35, 0x2C, 0x46),
        ),
        BackgroundWork::Cron { .. } => (
            Color32::from_rgb(0xF0, 0xC9, 0x8F),
            Color32::from_rgb(0x3F, 0x33, 0x26),
        ),
    };
    if dark_mode {
        BadgeColors { fg, bg }
    } else {
        BadgeColors { fg: bg, bg: fg }
    }
}

/// One entry's strings, as of `now` (Unix seconds).
fn build_row(entry: &BackgroundEntry, now: u64, i18n: &mut Localization) -> BackgroundRow {
    match &entry.work {
        BackgroundWork::Task(task) => task_row(&entry.id, entry.started_at, task, now, i18n),
        BackgroundWork::Wakeup {
            due_at,
            reason,
            prompt,
        } => {
            let title = if reason.is_empty() { prompt } else { reason };
            let when = if *due_at > now {
                let left = short_duration(due_at - now, true);
                tr!(
                    i18n,
                    "in {duration}",
                    "Dave session inspector background: time until a wake-up is due",
                    duration = left.as_str()
                )
            } else {
                tr!(
                    i18n,
                    "due",
                    "Dave session inspector background: a wake-up whose time has come"
                )
            };
            let at = super::clock_time(*due_at);
            let mut meta = tr!(
                i18n,
                "due {time}",
                "Dave session inspector background: when a wake-up is due",
                time = at.as_str()
            );
            if !reason.is_empty() && !prompt.is_empty() {
                meta.push_str(" · ");
                meta.push_str(prompt);
            }
            BackgroundRow {
                badge: tr!(
                    i18n,
                    "wake-up",
                    "Dave session inspector background: badge for a scheduled wake-up"
                ),
                title: title.clone(),
                title_is_code: false,
                when,
                meta,
            }
        }
        BackgroundWork::Cron {
            expr,
            prompt,
            recurring,
            durable,
        } => {
            let when = if *recurring {
                tr!(
                    i18n,
                    "recurring",
                    "Dave session inspector background: a cron job that fires on every match"
                )
            } else {
                tr!(
                    i18n,
                    "one-shot",
                    "Dave session inspector background: a cron job that fires once"
                )
            };
            let lifetime = if *durable {
                tr!(
                    i18n,
                    "durable",
                    "Dave session inspector background: a cron job saved to disk, outliving the CLI"
                )
            } else {
                tr!(
                    i18n,
                    "session-only",
                    "Dave session inspector background: a cron job that dies with the CLI"
                )
            };
            let job = tr!(
                i18n,
                "job {id}",
                "Dave session inspector background: a cron job's id",
                id = entry.id.as_str()
            );
            BackgroundRow {
                badge: tr!(
                    i18n,
                    "cron",
                    "Dave session inspector background: badge for a cron job"
                ),
                title: expr.clone(),
                title_is_code: true,
                when,
                meta: if prompt.is_empty() {
                    format!("{job} · {lifetime}")
                } else {
                    format!("{job} · {lifetime} · \"{prompt}\"")
                },
            }
        }
    }
}

/// A task's strings: its command (or description), age, and a meta line of
/// its id, output file, usage and progress.
fn task_row(
    id: &str,
    started_at: u64,
    task: &BackgroundTask,
    now: u64,
    i18n: &mut Localization,
) -> BackgroundRow {
    let title = task
        .command
        .as_deref()
        .filter(|c| !c.is_empty())
        .unwrap_or(&task.description);
    let mut meta = tr!(
        i18n,
        "task {id}",
        "Dave session inspector background: a background task's id",
        id = id
    );
    let mut part = |text: &str| {
        meta.push_str(" · ");
        meta.push_str(text);
    };
    // The description, when the command took the title.
    if task.command.is_some() && !task.description.is_empty() && task.description != title {
        part(&task.description);
    }
    if let Some(file) = &task.output_file {
        let name = std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file);
        part(name);
    }
    if let Some(usage) = task.usage.filter(|u| u.total_tokens > 0 || u.tool_uses > 0) {
        let tokens = format_tokens(usage.total_tokens);
        part(&tr!(
            i18n,
            "{tokens} tok",
            "Dave session inspector background: tokens a background task has used",
            tokens = tokens.as_str()
        ));
        part(&tr!(
            i18n,
            "{count} tool uses",
            "Dave session inspector background: tool calls a background task has made",
            count = usage.tool_uses
        ));
    }
    if let Some(tool) = &task.last_tool_name {
        part(&tr!(
            i18n,
            "last {tool}",
            "Dave session inspector background: the tool a background task called last",
            tool = tool.as_str()
        ));
    }
    if let Some(summary) = &task.summary {
        part(summary);
    }
    BackgroundRow {
        badge: kind_badge(task.kind, i18n),
        title_is_code: task
            .command
            .as_deref()
            .is_some_and(|c| c == title && !c.is_empty()),
        title: if title.is_empty() {
            id.to_string()
        } else {
            title.to_string()
        },
        when: short_duration(now.saturating_sub(started_at), false),
        meta,
    }
}

fn kind_badge(kind: BackgroundTaskKind, i18n: &mut Localization) -> String {
    match kind {
        BackgroundTaskKind::Bash => tr!(
            i18n,
            "bash",
            "Dave session inspector background: badge for a background shell command"
        ),
        BackgroundTaskKind::Monitor => tr!(
            i18n,
            "monitor",
            "Dave session inspector background: badge for a Monitor watching a command or socket"
        ),
        BackgroundTaskKind::Agent => tr!(
            i18n,
            "agent",
            "Dave session inspector background: badge for a background subagent"
        ),
        BackgroundTaskKind::Mcp => tr!(
            i18n,
            "mcp",
            "Dave session inspector background: badge for a long-running MCP tool call"
        ),
        BackgroundTaskKind::Workflow => tr!(
            i18n,
            "workflow",
            "Dave session inspector background: badge for a background workflow"
        ),
        BackgroundTaskKind::Other => tr!(
            i18n,
            "task",
            "Dave session inspector background: badge for a background task of an unknown kind"
        ),
    }
}

/// A span of seconds, compactly: "45s", "22m", "1h 5m", "2d 3h". With
/// `seconds`, minutes keep their seconds ("6m 12s"), for a countdown.
pub(super) fn short_duration(secs: u64, seconds: bool) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 && seconds {
        format!("{m}m {s}s")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// The status bar's Waiting pill: how many things will resume the session.
/// Shown only while it is Waiting; opens the Background card.
pub fn waiting_pill_ui(
    waiting_on: WaitingOn,
    memo: &mut LabelMemo<u32>,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> egui::Response {
    use crate::ui::badge::{BadgeVariant, StatusBadge};

    let wakeup = u32::from(waiting_on.wakeup_at.is_some());
    let count = waiting_on
        .background_tasks
        .saturating_add(waiting_on.crons)
        .saturating_add(wakeup);
    let text = memo.get(count, i18n, |count, i18n| {
        tr!(
            i18n,
            "Waiting · {count}",
            "Dave status bar pill: the session is parked on this many background tasks, wake-ups and crons",
            count = count
        )
    });
    StatusBadge::new(text)
        .variant(BadgeVariant::Purple)
        .show(ui)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_durations() {
        assert_eq!(short_duration(45, false), "45s");
        assert_eq!(short_duration(22 * 60 + 5, false), "22m");
        assert_eq!(short_duration(6 * 60 + 12, true), "6m 12s");
        assert_eq!(short_duration(3600 + 5 * 60, true), "1h 5m");
        assert_eq!(short_duration(2 * 86_400 + 3 * 3600, false), "2d 3h");
    }

    /// A task's row titles it by its command, and its meta line carries the
    /// description, output file name, usage and last tool.
    #[test]
    fn a_task_row_reads_its_command_and_progress() {
        let mut i18n = Localization::default();
        let entry = BackgroundEntry {
            id: "b7k2q".into(),
            started_at: 1000,
            work: BackgroundWork::Task(BackgroundTask {
                kind: BackgroundTaskKind::Bash,
                description: "wait for review".into(),
                command: Some("until headway show x; do sleep 60; done".into()),
                output_file: Some("/tmp/claude-1000/p/s/tasks/b7k2q.output".into()),
                usage: Some(crate::messages::TaskUsage {
                    total_tokens: 12_000,
                    tool_uses: 7,
                    duration_ms: 1,
                }),
                last_tool_name: Some("Grep".into()),
                summary: None,
                tool_use_id: None,
            }),
        };
        let row = build_row(&entry, 1000 + 22 * 60, &mut i18n);
        assert_eq!(row.badge, "bash");
        assert_eq!(row.title, "until headway show x; do sleep 60; done");
        assert_eq!(row.when, "22m");
        for part in [
            "b7k2q",
            "wait for review",
            "b7k2q.output",
            "12K",
            "7",
            "Grep",
        ] {
            assert!(row.meta.contains(part), "{part} in {:?}", row.meta);
        }
    }
}
