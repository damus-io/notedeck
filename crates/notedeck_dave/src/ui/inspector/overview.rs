//! The Overview card: what this session is and what it runs with.
//!
//! The top half reads off the session itself ([`SessionFacts`]): status,
//! model, mode, directory, refs and host. The bottom half is the CLI's
//! `system/init` as [`SessionInspector`] folded it: MCP servers as chips
//! coloured by status, then counts of tools, skills, plugins and agent types,
//! each of which expands to its list.

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::inspector::{InspectorView, LabelMemo, OverviewLabels, OverviewList, SessionInspector};
use crate::messages::{CliState, McpServerStatus, SessionInfo, WaitingOn};
use crate::session::SessionDetails;
use crate::ui::badge::{BadgeVariant, StatusBadge};
use crate::ui::dave::permission_mode_badge;
use claude_agent_sdk_rs::PermissionMode;
use notedeck::{tr, tr_plural, Localization};
use notedeck_ui::app_images;

use super::{clock_time, heading_ui, key_ui, mono_ui, value_ui, weak_ui, MONO_SIZE, VALUE_SIZE};

/// What the Overview reads off the session rather than the inspector: state
/// the session already keeps for the header, status bar and sidebar.
pub struct SessionFacts<'a> {
    pub status: AgentStatus,
    /// What will resume a Waiting session.
    pub waiting_on: WaitingOn,
    pub permission_mode: PermissionMode,
    pub backend_type: BackendType,
    pub details: Option<&'a SessionDetails>,
    /// The git branch of the session's cwd, local sessions only.
    pub branch: Option<&'a str>,
    /// The Claude CLI session id, for `--resume`.
    pub cli_session_id: Option<&'a str>,
    /// The session runs on another host, whose CLI process this one can't see.
    pub is_remote: bool,
    /// The model's context window, in tokens, for the Usage card.
    pub context_window: u64,
    /// The Claude account's rate-limit windows, for the Rate limits card.
    pub rate_limit: Option<crate::messages::RateLimitInfo>,
    /// Now, in Unix seconds, for ages and countdowns.
    pub now_unix: u64,
    /// The transcript, whose subagent rows the Subagents card lists.
    pub chat: &'a [crate::Message],
    /// The allowlist and recorded decisions the Permissions card reads.
    pub permissions: Option<super::PermissionFacts<'a>>,
}

/// The Overview card's body.
pub fn overview_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    view: &mut InspectorView,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    egui::Grid::new("dave_inspector_overview")
        .num_columns(2)
        .spacing(egui::vec2(
            notedeck::tokens::SPACING_MD,
            notedeck::tokens::SPACING_XS,
        ))
        .show(ui, |ui| facts_rows_ui(inspector, facts, i18n, ui));

    ui.add_space(notedeck::tokens::SPACING_SM);
    let Some(info) = &inspector.session_info else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No init from the agent yet",
                "Dave session inspector overview: shown before the CLI's system/init message arrives"
            ),
        );
        return;
    };
    mcp_servers_ui(info, i18n, ui);
    count_chips_ui(info, &mut inspector.labels, view, i18n, ui);
    if let Some(list) = view.overview_list {
        list_ui(info, list, ui);
    }
    plugin_errors_ui(info, ui);
}

/// The key/value rows: status down to the CLI process.
fn facts_rows_ui(
    inspector: &mut SessionInspector,
    facts: &SessionFacts<'_>,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let info = inspector.session_info.as_ref();

    key_ui(
        ui,
        &tr!(i18n, "Status", "Dave session inspector overview row"),
    );
    ui.horizontal(|ui| {
        status_dot_ui(ui, facts.status.color());
        value_ui(ui, facts.status.label());
        if facts.status == AgentStatus::Waiting {
            let waiting = inspector
                .labels
                .waiting
                .get(facts.waiting_on, i18n, waiting_text);
            weak_ui(ui, waiting);
        }
    });
    ui.end_row();

    let model = facts
        .details
        .and_then(|d| d.model.as_deref())
        .or_else(|| info.and_then(|i| i.model.as_deref()));
    if let Some(model) = model {
        key_ui(
            ui,
            &tr!(i18n, "Model", "Dave session inspector overview row"),
        );
        mono_ui(ui, model);
        ui.end_row();
    }

    if let Some(version) = info.and_then(|i| i.cli_version.as_deref()) {
        key_ui(ui, facts.backend_type.display_name());
        ui.horizontal(|ui| {
            mono_ui(ui, version);
            if let Some(style) = info.and_then(|i| i.output_style.as_deref()) {
                weak_ui(
                    ui,
                    &tr!(
                        i18n,
                        "output style",
                        "Dave session inspector overview: label before the CLI's output style name"
                    ),
                );
                mono_ui(ui, style);
            }
        });
        ui.end_row();
    }

    key_ui(
        ui,
        &tr!(i18n, "Mode", "Dave session inspector overview row"),
    );
    permission_mode_badge(facts.permission_mode).show(ui);
    ui.end_row();

    if let Some(cwd) = facts.details.and_then(|d| d.cwd.as_deref()) {
        key_ui(
            ui,
            &tr!(i18n, "Directory", "Dave session inspector overview row"),
        );
        ui.horizontal_wrapped(|ui| {
            mono_ui(ui, &cwd.to_string_lossy());
            if let Some(branch) = facts.branch {
                weak_ui(ui, branch);
            }
        });
        ui.end_row();
    }

    if let Some(session_ref) = &inspector.session_ref {
        key_ui(
            ui,
            &tr!(i18n, "Session", "Dave session inspector overview row"),
        );
        mono_ui(ui, session_ref);
        ui.end_row();
    }

    if let Some(id) = facts.cli_session_id {
        key_ui(
            ui,
            &tr!(
                i18n,
                "CLI session",
                "Dave session inspector overview row: the agent CLI's own session id"
            ),
        );
        ui.horizontal(|ui| {
            mono_ui(ui, id);
            let copy = copy_button_ui(ui).on_hover_text(tr!(
                i18n,
                "Copy",
                "Dave session inspector: copy a value"
            ));
            if copy.clicked() {
                ui.ctx().copy_text(id.to_owned());
            }
        });
        ui.end_row();
    }

    key_ui(
        ui,
        &tr!(i18n, "Host", "Dave session inspector overview row"),
    );
    match facts.details.map(|d| d.hostname.as_str()) {
        Some(host) if !host.is_empty() => mono_ui(ui, host),
        _ => weak_ui(
            ui,
            &tr!(
                i18n,
                "this machine",
                "Dave session inspector overview: host of a local session"
            ),
        ),
    }
    ui.end_row();

    if facts.backend_type == BackendType::Claude && !facts.is_remote {
        key_ui(
            ui,
            &tr!(i18n, "Process", "Dave session inspector overview row"),
        );
        let cli = inspector.cli.unwrap_or_default();
        value_ui(ui, inspector.labels.cli.get(cli, i18n, cli_text));
        ui.end_row();
    }

    if let Some(source) = info.and_then(|i| i.api_key_source.as_deref()) {
        key_ui(
            ui,
            &tr!(
                i18n,
                "Key source",
                "Dave session inspector overview row: where the CLI got its API credentials"
            ),
        );
        mono_ui(ui, source);
        ui.end_row();
    }
}

/// One chip per MCP server, coloured by whether it connected.
fn mcp_servers_ui(info: &SessionInfo, i18n: &mut Localization, ui: &mut egui::Ui) {
    if info.mcp_servers.is_empty() {
        return;
    }
    heading_ui(
        ui,
        &tr!(
            i18n,
            "MCP servers",
            "Dave session inspector overview heading"
        ),
    );
    ui.horizontal_wrapped(|ui| {
        for server in &info.mcp_servers {
            let chip = StatusBadge::new(&server.name)
                .variant(mcp_variant(server.status))
                .show(ui);
            chip.on_hover_text(mcp_status_wire(server.status));
        }
    });
}

/// The count chips: tools, skills, plugins and agent types. A click opens
/// that chip's list beneath them.
fn count_chips_ui(
    info: &SessionInfo,
    labels: &mut OverviewLabels,
    view: &mut InspectorView,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    type Plural = fn(usize, &mut Localization) -> String;
    let chips: [(OverviewList, usize, &mut LabelMemo<usize>, Plural); 4] = [
        (
            OverviewList::Tools,
            info.tools.len(),
            &mut labels.tools,
            |n, i18n| {
                tr_plural!(
                    i18n,
                    "{count} tool",
                    "{count} tools",
                    "Dave session inspector: count chip of the agent's tools",
                    n as i64,
                )
            },
        ),
        (
            OverviewList::Skills,
            info.skills.len(),
            &mut labels.skills,
            |n, i18n| {
                tr_plural!(
                    i18n,
                    "{count} skill",
                    "{count} skills",
                    "Dave session inspector: count chip of the agent's skills",
                    n as i64,
                )
            },
        ),
        (
            OverviewList::Plugins,
            info.plugins.len(),
            &mut labels.plugins,
            |n, i18n| {
                tr_plural!(
                    i18n,
                    "{count} plugin",
                    "{count} plugins",
                    "Dave session inspector: count chip of the agent's plugins",
                    n as i64,
                )
            },
        ),
        (
            OverviewList::Agents,
            info.agents.len(),
            &mut labels.agents,
            |n, i18n| {
                tr_plural!(
                    i18n,
                    "{count} agent type",
                    "{count} agent types",
                    "Dave session inspector: count chip of the subagent types the agent can spawn",
                    n as i64,
                )
            },
        ),
    ];
    ui.add_space(notedeck::tokens::SPACING_XS);
    ui.horizontal_wrapped(|ui| {
        for (list, count, memo, plural) in chips {
            let open = view.overview_list == Some(list);
            let text = egui::RichText::new(memo.get(count, i18n, plural))
                .font(egui::FontId::proportional(VALUE_SIZE));
            if ui.add(egui::Button::selectable(open, text)).clicked() {
                view.toggle_overview_list(list);
            }
        }
    });
}

/// The list behind an expanded count chip.
fn list_ui(info: &SessionInfo, list: OverviewList, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| match list {
        OverviewList::Tools => info.tools.iter().for_each(|t| mono_ui(ui, t)),
        OverviewList::Skills => info.skills.iter().for_each(|s| mono_ui(ui, s)),
        OverviewList::Agents => info.agents.iter().for_each(|a| mono_ui(ui, a)),
        OverviewList::Plugins => {
            for plugin in &info.plugins {
                mono_ui(ui, &plugin.name);
                if let Some(version) = &plugin.version {
                    weak_ui(ui, version);
                }
            }
        }
    });
}

/// Plugins the CLI failed to load, one line each.
fn plugin_errors_ui(info: &SessionInfo, ui: &mut egui::Ui) {
    let color = ui.visuals().error_fg_color;
    for err in &info.plugin_errors {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(&err.plugin)
                    .font(egui::FontId::monospace(MONO_SIZE))
                    .color(color),
            );
            ui.label(
                egui::RichText::new(&err.message)
                    .font(egui::FontId::proportional(VALUE_SIZE))
                    .color(color),
            );
        });
    }
}

/// A status's colour as a small filled circle, like the session list's.
fn status_dot_ui(ui: &mut egui::Ui, color: egui::Color32) {
    let size = egui::vec2(8.0, 8.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A small copy-to-clipboard icon button, tinted for the theme.
fn copy_button_ui(ui: &mut egui::Ui) -> egui::Response {
    let image = if ui.visuals().dark_mode {
        app_images::copy_to_clipboard_image()
    } else {
        app_images::copy_to_clipboard_dark_image()
    };
    ui.add(egui::Button::image(image.max_size(egui::vec2(12.0, 12.0))).frame(false))
}

/// The chip colour for an MCP server's status.
fn mcp_variant(status: McpServerStatus) -> BadgeVariant {
    match status {
        McpServerStatus::Connected => BadgeVariant::Success,
        McpServerStatus::Failed => BadgeVariant::Destructive,
        McpServerStatus::NeedsAuth => BadgeVariant::Warning,
        McpServerStatus::Pending | McpServerStatus::Disabled | McpServerStatus::Unknown => {
            BadgeVariant::Default
        }
    }
}

/// An MCP server's status as the CLI spells it, for the chip's hover.
fn mcp_status_wire(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Connected => "connected",
        McpServerStatus::Failed => "failed",
        McpServerStatus::NeedsAuth => "needs-auth",
        McpServerStatus::Pending => "pending",
        McpServerStatus::Disabled => "disabled",
        McpServerStatus::Unknown => "unknown",
    }
}

/// What a Waiting session is parked on: "2 background tasks · wake-up at
/// 3:42 PM · 1 cron".
fn waiting_text(on: WaitingOn, i18n: &mut Localization) -> String {
    let mut parts = Vec::new();
    if on.background_tasks > 0 {
        parts.push(tr_plural!(
            i18n,
            "{count} background task",
            "{count} background tasks",
            "Dave session inspector: what a Waiting session is parked on",
            on.background_tasks as i64,
        ));
    }
    if let Some(at) = on.wakeup_at {
        parts.push(tr!(
            i18n,
            "wake-up at {time}",
            "Dave session inspector: a Waiting session's scheduled wake-up time",
            time = clock_time(at)
        ));
    }
    if on.crons > 0 {
        parts.push(tr_plural!(
            i18n,
            "{count} cron",
            "{count} crons",
            "Dave session inspector: what a Waiting session is parked on",
            on.crons as i64,
        ));
    }
    parts.join(" · ")
}

/// The CLI process line: running (and when the idle reaper stops it), or
/// stopped until the next message.
fn cli_text(cli: CliState, i18n: &mut Localization) -> String {
    match (cli.connected, cli.reap_at) {
        (false, _) => tr!(
            i18n,
            "Stopped · the next message starts it",
            "Dave session inspector: the agent CLI process is not running"
        ),
        (true, None) => tr!(
            i18n,
            "Running · held open by work in flight",
            "Dave session inspector: the agent CLI process is running and busy, so the idle reaper won't stop it"
        ),
        (true, Some(at)) => tr!(
            i18n,
            "Running · stops if idle until {time}",
            "Dave session inspector: the agent CLI process is running and the idle reaper stops it at this time",
            time = clock_time(at)
        ),
    }
}
