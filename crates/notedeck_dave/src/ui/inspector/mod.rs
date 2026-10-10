//! The session view's Details tab: a rail of sections beside a scrolling grid
//! of section cards, plus the `Chat | Details` tab strip above it.
//!
//! State lives in [`crate::inspector`]; this module only draws it. Each section
//! card names its data source in small mono text, so a missing number is easy
//! to trace back to where it should have come from.

mod background;
mod files;
mod overview;
mod permissions;
mod rate_limits;
mod subagents;
mod tasks;
mod usage;

pub use background::waiting_pill_ui;
pub use overview::SessionFacts;
pub use permissions::PermissionFacts;
pub use rate_limits::rate_limit_pill_ui;

use crate::inspector::{InspectorSection, InspectorView, SessionInspector, SessionTab};
use crate::ui::DaveAction;
use notedeck::{tr, Localization};

/// Width of the section rail on wide layouts.
const RAIL_WIDTH: f32 = 168.0;

/// Narrowest a section card gets before the grid drops a column.
const CARD_MIN_WIDTH: f32 = 330.0;

/// Size of the small mono text: chord labels and data sources.
const MONO_SIZE: f32 = 10.5;

/// Size of a card's proportional text: values, status, notes. Between the
/// small keys and body text, so it sits level with the mono values.
const VALUE_SIZE: f32 = 12.5;

/// Gap between section cards, both ways.
const CARD_GAP: f32 = notedeck::tokens::SPACING_MD;

/// The `Chat | Details` tab strip under the session header. Each tab shows the
/// normal-mode chord that switches to it.
pub fn session_tabs_ui(
    view: &InspectorView,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        let chat = tr!(i18n, "Chat", "Dave session tab: the chat transcript");
        if tab_ui(ui, &chat, "g c", view.tab == SessionTab::Chat).clicked() {
            action = Some(DaveAction::ShowChat);
        }
        let details = tr!(
            i18n,
            "Details",
            "Dave session tab: the session inspector (usage, background work, files, …)"
        );
        if tab_ui(ui, &details, "g d", view.tab == SessionTab::Details).clicked() {
            action = Some(DaveAction::OpenInspector(view.section));
        }
    });
    action
}

/// One tab: a selectable label, its chord in weak mono beside it, and an
/// underline when selected.
fn tab_ui(ui: &mut egui::Ui, label: &str, keys: &str, selected: bool) -> egui::Response {
    let resp = ui.add(egui::Button::selectable(selected, label));
    ui.label(
        egui::RichText::new(keys)
            .font(egui::FontId::monospace(MONO_SIZE))
            .color(ui.visuals().weak_text_color()),
    );
    if selected {
        let y = resp.rect.bottom() + 1.0;
        ui.painter().hline(
            resp.rect.x_range(),
            y,
            egui::Stroke::new(
                notedeck::tokens::STROKE_THICK,
                ui.visuals().selection.stroke.color,
            ),
        );
    }
    resp
}

/// The Details tab: the section rail (wide layouts only) and the card grid.
pub fn inspector_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    view: &mut InspectorView,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let mut action = None;
    let show_rail = !notedeck::ui::is_narrow(ui.ctx());

    ui.horizontal_top(|ui| {
        if show_rail {
            action = rail_ui(view.section, i18n, ui);
            let x = ui.min_rect().right();
            ui.painter().vline(
                x,
                ui.max_rect().y_range(),
                ui.visuals().widgets.noninteractive.bg_stroke,
            );
        }
        if let Some(card_action) = card_grid_ui(inspector, facts, view, i18n, ui) {
            action = Some(card_action);
        }
    });

    action
}

/// The section index down the left: one entry per section, the selected one
/// marked. A click scrolls the grid to that section's card.
fn rail_ui(
    selected: InspectorSection,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let mut action = None;
    ui.add_space(notedeck::tokens::SPACING_SM);
    ui.allocate_ui_with_layout(
        egui::vec2(RAIL_WIDTH, ui.available_height()),
        egui::Layout::top_down_justified(egui::Align::Min),
        |ui| {
            ui.set_width(RAIL_WIDTH);
            ui.add_space(notedeck::tokens::SPACING_LG);
            for section in InspectorSection::ALL {
                let title = section_title(section, i18n);
                if ui
                    .add(egui::Button::selectable(section == selected, title))
                    .clicked()
                {
                    action = Some(DaveAction::OpenInspector(section));
                }
            }
        },
    );
    action
}

/// The section cards, as many columns as fit, scrolling vertically.
fn card_grid_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    view: &mut InspectorView,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let scroll_to = view.take_scroll();
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("dave_inspector_grid")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_LG as i8))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(CARD_GAP, CARD_GAP);
                    let width = ui.available_width();
                    let columns = (((width + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor()
                        as usize)
                        .max(1);
                    let card_width =
                        ((width - CARD_GAP * (columns - 1) as f32) / columns as f32).max(0.0);

                    // The rows stack; the scroll area's content would
                    // otherwise inherit the rail's horizontal layout.
                    ui.vertical(|ui| {
                        for row in InspectorSection::ALL.chunks(columns) {
                            ui.horizontal_top(|ui| {
                                for &section in row {
                                    let card = section_card_ui(
                                        inspector, facts, view, section, card_width, i18n, ui,
                                    );
                                    if scroll_to == Some(section) {
                                        card.response.scroll_to_me(Some(egui::Align::TOP));
                                    }
                                    if card.inner.is_some() {
                                        action = card.inner;
                                    }
                                }
                            });
                        }
                    });
                });
        });
    action
}

/// One section's card: its title and data source over its body, and what a
/// click in the body asked for.
#[allow(clippy::too_many_arguments)]
fn section_card_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    view: &mut InspectorView,
    section: InspectorSection,
    width: f32,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> egui::InnerResponse<Option<DaveAction>> {
    let margin = notedeck::tokens::SPACING_MD;
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(notedeck::tokens::RADIUS_MD)
        .inner_margin(egui::Margin::same(margin as i8))
        .show(ui, |ui| {
            ui.set_width(width - 2.0 * margin);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(section_title(section, i18n))
                            .small()
                            .strong()
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(section_source(section))
                                .font(egui::FontId::monospace(MONO_SIZE))
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
                section_body_ui(inspector, facts, view, section, i18n, ui)
            })
            .inner
        })
}

/// A section's contents, and what a click in them asked for. A section
/// without a view yet is a stub until its card fills it in.
fn section_body_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    view: &mut InspectorView,
    section: InspectorSection,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    match section {
        InspectorSection::Subagents => {
            return subagents::subagents_ui(&mut inspector.subagents, facts.chat, i18n, ui)
        }
        InspectorSection::Overview => overview::overview_ui(inspector, facts, view, i18n, ui),
        InspectorSection::Usage => usage::usage_ui(inspector, facts.context_window, i18n, ui),
        InspectorSection::RateLimits => {
            rate_limits::rate_limits_ui(inspector, facts.rate_limit.as_ref(), i18n, ui)
        }
        InspectorSection::Background => background::background_ui(
            &mut inspector.background,
            facts.is_remote,
            facts.now_unix,
            i18n,
            ui,
        ),
        InspectorSection::Tasks => {
            tasks::tasks_ui(&mut inspector.tasks_header, facts.chat, i18n, ui)
        }
        InspectorSection::Files => {
            return files::files_ui(&mut inspector.files, &mut inspector.edited, facts, i18n, ui)
        }
        InspectorSection::Permissions => {
            return permissions::permissions_ui(
                &mut inspector.permissions,
                facts.permission_mode,
                facts.permissions,
                facts.chat,
                facts.is_remote,
                i18n,
                ui,
            )
        }
        _ => empty_state_ui(i18n, ui),
    }
    None
}

/// What a section shows before anything has reached it.
fn empty_state_ui(i18n: &mut Localization, ui: &mut egui::Ui) {
    ui.label(
        egui::RichText::new(tr!(
            i18n,
            "Nothing yet",
            "Dave session inspector: empty state of a section with no data"
        ))
        .weak(),
    );
}

/// A section's name, as the rail and its card title show it.
pub fn section_title(section: InspectorSection, i18n: &mut Localization) -> String {
    match section {
        InspectorSection::Overview => tr!(
            i18n,
            "Overview",
            "Dave session inspector section: model, versions, MCP servers, skills"
        ),
        InspectorSection::Usage => tr!(
            i18n,
            "Usage",
            "Dave session inspector section: context, tokens and cost"
        ),
        InspectorSection::RateLimits => tr!(
            i18n,
            "Rate limits",
            "Dave session inspector section: the account's rate-limit windows"
        ),
        InspectorSection::Background => tr!(
            i18n,
            "Background",
            "Dave session inspector section: background tasks, monitors, wake-ups and crons"
        ),
        InspectorSection::Subagents => tr!(
            i18n,
            "Subagents",
            "Dave session inspector section: subagents the session spawned"
        ),
        InspectorSection::Tasks => tr!(
            i18n,
            "Tasks",
            "Dave session inspector section: the agent's task list"
        ),
        InspectorSection::Files => tr!(
            i18n,
            "Files",
            "Dave session inspector section: scratchpad, task output and edited files"
        ),
        InspectorSection::Permissions => tr!(
            i18n,
            "Permissions",
            "Dave session inspector section: permission mode, answered prompts, always-allow rules"
        ),
        InspectorSection::Outputs => tr!(
            i18n,
            "Outputs",
            "Dave session inspector section: commits, cards and pages the session produced"
        ),
    }
}

/// Where a section's data comes from, shown in mono on its card. Message and
/// type names, so not translated.
fn section_source(section: InspectorSection) -> &'static str {
    match section {
        InspectorSection::Overview => "SessionInfo · system/init",
        InspectorSection::Usage => "UsageUpdate · QueryComplete · CompactionComplete",
        InspectorSection::RateLimits => "rate_limit_event",
        InspectorSection::Background => "background_tasks_changed · task_progress",
        InspectorSection::Subagents => "SubagentInfo · task_progress",
        InspectorSection::Tasks => "TaskCreate/TaskUpdate · TodoWrite",
        InspectorSection::Files => "session temp dir · FileUpdate",
        InspectorSection::Permissions => "runtime allowlist · PermissionResponse",
        InspectorSection::Outputs => "git log --grep · headway refs",
    }
}

/// A row's key, in weak small text.
fn key_ui(ui: &mut egui::Ui, key: &str) {
    ui.label(egui::RichText::new(key).small().weak());
}

/// A small weak heading over a group of chips.
fn heading_ui(ui: &mut egui::Ui, text: &str) {
    ui.add_space(notedeck::tokens::SPACING_XS);
    ui.label(egui::RichText::new(text).small().weak());
}

/// A value in small mono text: ids, versions, paths.
fn mono_ui(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(egui::FontId::monospace(MONO_SIZE)));
}

/// A value in proportional text.
fn value_ui(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(egui::FontId::proportional(VALUE_SIZE)));
}

/// A secondary value beside another.
fn weak_ui(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(egui::FontId::proportional(VALUE_SIZE))
            .weak(),
    );
}

/// A Unix time as a local wall-clock time, like "3:42 PM".
fn clock_time(unix: u64) -> String {
    chrono::DateTime::from_timestamp(unix as i64, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%-I:%M %p")
                .to_string()
        })
        .unwrap_or_default()
}
