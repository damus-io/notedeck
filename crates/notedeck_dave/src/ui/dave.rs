use super::badge::{BadgeVariant, StatusBadge};
use super::diff;
use super::git_status_ui;
use super::markdown_ui;
use super::query_ui::query_call_ui;
use super::top_buttons::top_buttons_ui;
use crate::{
    backend::BackendType,
    config::{AiMode, DaveSettings},
    file_update::FileUpdate,
    git_status::GitStatusCache,
    messages::{
        AskUserQuestionInput, AssistantMessage, CompactionInfo, ExecutedTool, Message,
        PermissionRequest, PermissionResponse, PermissionResponseType, QuestionAnswer,
        SubagentInfo, SubagentStatus,
    },
    session::{PermissionMessageState, SessionDetails, SessionId},
    tools::{PresentNotesCall, ToolCall, ToolCalls, ToolResponse, ToolResponses},
};
use bitflags::bitflags;
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::Transaction;
use notedeck::{tr, AppContext, Localization, NoteAction, NoteContext};
use notedeck_ui::{icons::search_icon, NoteOptions};
use std::collections::HashMap;
use uuid::Uuid;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DaveUiFlags: u16 {
        const Trial            = 1 << 0;
        const Compact          = 1 << 1;
        const IsWorking        = 1 << 2;
        const InterruptPending = 1 << 3;
        const HasPendingPerm   = 1 << 4;
        const PlanModeActive   = 1 << 5;
        const IsCompacting     = 1 << 6;
        const AutoStealFocus   = 1 << 7;
        const IsRemote         = 1 << 8;
    }
}

/// DaveUi holds all of the data it needs to render itself
pub struct DaveUi<'a> {
    chat: &'a [Message],
    flags: DaveUiFlags,
    input: &'a mut String,
    focus_requested: &'a mut bool,
    /// Session ID for per-session scroll state
    session_id: SessionId,
    /// State for tentative permission response (waiting for message)
    permission_message_state: PermissionMessageState,
    /// State for AskUserQuestion responses (selected options per question)
    question_answers: Option<&'a mut HashMap<Uuid, Vec<QuestionAnswer>>>,
    /// Current question index for multi-question AskUserQuestion
    question_index: Option<&'a mut HashMap<Uuid, usize>>,
    /// AI interaction mode (Chat vs Agentic)
    ai_mode: AiMode,
    /// Git status cache for current session (agentic only)
    git_status: Option<&'a mut GitStatusCache>,
    /// Session details for header display
    details: Option<&'a SessionDetails>,
    /// Color for the notification dot on the mobile hamburger icon,
    /// derived from FocusPriority of the next focus queue entry.
    status_dot_color: Option<egui::Color32>,
    /// Usage metrics for the current session (tokens, cost)
    usage: Option<&'a crate::messages::UsageInfo>,
    /// Context window size for the current model
    context_window: u64,
    /// Dispatch lifecycle state, used for queued indicator logic.
    dispatch_state: crate::session::DispatchState,
    /// Which backend this session uses
    backend_type: BackendType,
}

/// The response the app generates. The response contains an optional
/// action to take.
#[derive(Default, Debug)]
pub struct DaveResponse {
    pub action: Option<DaveAction>,
}

impl DaveResponse {
    pub fn new(action: DaveAction) -> Self {
        DaveResponse {
            action: Some(action),
        }
    }

    fn note(action: NoteAction) -> DaveResponse {
        Self::new(DaveAction::Note(action))
    }

    pub fn or(self, r: DaveResponse) -> DaveResponse {
        DaveResponse {
            action: self.action.or(r.action),
        }
    }

    /// Generate a send response to the controller
    fn send() -> Self {
        Self::new(DaveAction::Send)
    }

    fn none() -> Self {
        DaveResponse::default()
    }
}

/// The actions the app generates. No default action is specfied in the
/// UI code. This is handled by the app logic, however it chooses to
/// process this message.
#[derive(Debug)]
pub enum DaveAction {
    /// The action generated when the user sends a message to dave
    Send,
    NewChat,
    ToggleChrome,
    Note(NoteAction),
    /// Toggle showing the session list (for mobile navigation)
    ShowSessionList,
    /// Open the settings panel
    OpenSettings,
    /// Settings were updated and should be persisted
    UpdateSettings(DaveSettings),
    /// User responded to a permission request
    PermissionResponse {
        request_id: Uuid,
        response: PermissionResponse,
    },
    /// User wants to interrupt/stop the current AI operation
    Interrupt,
    /// Enter tentative accept mode (Shift+click on Yes)
    TentativeAccept,
    /// Enter tentative deny mode (Shift+click on No)
    TentativeDeny,
    /// User responded to an AskUserQuestion
    QuestionResponse {
        request_id: Uuid,
        answers: Vec<QuestionAnswer>,
    },
    /// User approved or rejected an ExitPlanMode request
    ExitPlanMode {
        request_id: Uuid,
        approved: bool,
    },
    /// User approved plan and wants to compact first
    CompactAndApprove {
        request_id: Uuid,
    },
    /// Toggle plan mode (clicked PLAN badge)
    TogglePlanMode,
    /// Toggle auto-steal focus mode (clicked AUTO badge)
    ToggleAutoSteal,
}

impl<'a> DaveUi<'a> {
    pub fn new(
        trial: bool,
        session_id: SessionId,
        chat: &'a [Message],
        input: &'a mut String,
        focus_requested: &'a mut bool,
        ai_mode: AiMode,
    ) -> Self {
        let flags = if trial {
            DaveUiFlags::Trial
        } else {
            DaveUiFlags::empty()
        };
        DaveUi {
            flags,
            session_id,
            chat,
            input,
            focus_requested,
            permission_message_state: PermissionMessageState::None,
            question_answers: None,
            question_index: None,
            ai_mode,
            git_status: None,
            details: None,
            status_dot_color: None,
            usage: None,
            context_window: crate::messages::context_window_for_model(None),
            dispatch_state: crate::session::DispatchState::default(),
            backend_type: BackendType::Remote,
        }
    }

    pub fn backend_type(mut self, bt: BackendType) -> Self {
        self.backend_type = bt;
        self
    }

    pub fn details(mut self, details: &'a SessionDetails) -> Self {
        self.details = Some(details);
        self
    }

    pub fn permission_message_state(mut self, state: PermissionMessageState) -> Self {
        self.permission_message_state = state;
        self
    }

    pub fn question_answers(mut self, answers: &'a mut HashMap<Uuid, Vec<QuestionAnswer>>) -> Self {
        self.question_answers = Some(answers);
        self
    }

    pub fn question_index(mut self, index: &'a mut HashMap<Uuid, usize>) -> Self {
        self.question_index = Some(index);
        self
    }

    pub fn compact(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::Compact, val);
        self
    }

    pub fn is_working(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsWorking, val);
        self
    }

    pub fn dispatch_state(mut self, state: crate::session::DispatchState) -> Self {
        self.dispatch_state = state;
        self
    }

    pub fn interrupt_pending(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::InterruptPending, val);
        self
    }

    pub fn has_pending_permission(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::HasPendingPerm, val);
        self
    }

    pub fn plan_mode_active(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::PlanModeActive, val);
        self
    }

    pub fn is_compacting(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsCompacting, val);
        self
    }

    /// Set the git status cache. Mutable because the UI toggles
    /// expand/collapse and triggers refresh on button click.
    pub fn git_status(mut self, cache: &'a mut GitStatusCache) -> Self {
        self.git_status = Some(cache);
        self
    }

    pub fn auto_steal_focus(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::AutoStealFocus, val);
        self
    }

    pub fn is_remote(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsRemote, val);
        self
    }

    pub fn status_dot_color(mut self, color: Option<egui::Color32>) -> Self {
        self.status_dot_color = color;
        self
    }

    pub fn usage(mut self, usage: &'a crate::messages::UsageInfo, model: Option<&str>) -> Self {
        self.usage = Some(usage);
        self.context_window = crate::messages::context_window_for_model(model);
        self
    }

    fn chat_margin(&self, ctx: &egui::Context) -> i8 {
        if self.flags.contains(DaveUiFlags::Compact) || notedeck::ui::is_narrow(ctx) {
            8
        } else {
            20
        }
    }

    fn chat_frame(&self, ctx: &egui::Context) -> egui::Frame {
        let margin = self.chat_margin(ctx);
        egui::Frame::new().inner_margin(egui::Margin {
            left: margin,
            right: margin,
            top: 50,
            bottom: 0,
        })
    }

    /// The main render function. Call this to render Dave
    pub fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Override Truncate wrap mode that StripBuilder sets when clip=true
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

        let is_compact = self.flags.contains(DaveUiFlags::Compact);

        // Skip top buttons in compact mode (scene panel has its own controls)
        let action = if is_compact {
            None
        } else {
            let result = top_buttons_ui(app_ctx, ui, self.status_dot_color);

            // Render session details inline, to the right of the buttons
            if let Some(details) = self.details {
                let available_width = ui.available_width();
                let max_width = available_width - result.right_edge_x;
                if max_width > 50.0 {
                    let details_rect = egui::Rect::from_min_size(
                        egui::pos2(result.right_edge_x, result.y),
                        egui::vec2(max_width, 32.0),
                    );
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(details_rect), |ui| {
                        ui.set_clip_rect(details_rect);
                        session_header_ui(ui, details, self.backend_type);
                    });
                }
            }

            result.action
        };

        egui::Frame::NONE
            .show(ui, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let margin = self.chat_margin(ui.ctx());
                    let bottom_margin = 100;

                    let mut r = egui::Frame::new()
                        .outer_margin(egui::Margin {
                            left: margin,
                            right: margin,
                            top: 0,
                            bottom: bottom_margin,
                        })
                        .inner_margin(egui::Margin::same(8))
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(12.0)
                        .show(ui, |ui| self.inputbox(app_ctx, ui))
                        .inner;

                    {
                        let plan_mode_active = self.flags.contains(DaveUiFlags::PlanModeActive);
                        let auto_steal_focus = self.flags.contains(DaveUiFlags::AutoStealFocus);
                        let is_agentic = self.ai_mode == AiMode::Agentic;
                        let has_git = self.git_status.is_some();

                        // Show status bar when there's git status or badges to display
                        if has_git || is_agentic {
                            // Explicitly reserve height so bottom_up layout
                            // keeps the chat ScrollArea from overlapping.
                            let h = if self.git_status.as_ref().is_some_and(|gs| gs.expanded) {
                                200.0
                            } else {
                                24.0
                            };
                            let w = ui.available_width();
                            let badge_action = ui
                                .allocate_ui(egui::vec2(w, h), |ui| {
                                    egui::Frame::new()
                                        .outer_margin(egui::Margin {
                                            left: margin,
                                            right: margin,
                                            top: 4,
                                            bottom: 0,
                                        })
                                        .show(ui, |ui| {
                                            status_bar_ui(
                                                self.git_status.as_deref_mut(),
                                                is_agentic,
                                                plan_mode_active,
                                                auto_steal_focus,
                                                self.usage,
                                                self.context_window,
                                                ui,
                                            )
                                        })
                                        .inner
                                })
                                .inner;

                            if let Some(action) = badge_action {
                                r = DaveResponse::new(action).or(r);
                            }
                        }
                    }

                    let chat_response = egui::ScrollArea::vertical()
                        .id_salt(("dave_chat_scroll", self.session_id))
                        .stick_to_bottom(true)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            self.chat_frame(ui.ctx())
                                .show(ui, |ui| {
                                    ui.vertical(|ui| self.render_chat(app_ctx, ui)).inner
                                })
                                .inner
                        })
                        .inner;

                    chat_response.or(r)
                })
                .inner
            })
            .inner
            .or(DaveResponse { action })
    }

    fn error_chat(&self, i18n: &mut Localization, err: &str, ui: &mut egui::Ui) {
        if self.flags.contains(DaveUiFlags::Trial) {
            ui.add(egui::Label::new(
                egui::RichText::new(
                    tr!(i18n, "The Dave Nostr AI assistant trial has ended :(. Thanks for testing! Zap-enabled Dave coming soon!", "Message shown when Dave trial period has ended"),
                )
                .weak(),
            ));
        } else {
            ui.add(egui::Label::new(
                egui::RichText::new(format!("An error occured: {err}")).weak(),
            ));
        }
    }

    /// Render a chat message (user, assistant, tool call/response, etc)
    fn render_chat(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let mut response = DaveResponse::default();
        let is_agentic = self.ai_mode == AiMode::Agentic;

        // Find where queued (not-yet-dispatched) user messages start.
        // When streaming, append_token inserts an Assistant between the
        // dispatched User and any queued Users, so all trailing Users
        // after that Assistant are queued. Before the first token arrives
        // there's no Assistant yet, so we skip the dispatched count
        // trailing Users (they were all sent in the prompt).
        let queued_from = if self.flags.contains(DaveUiFlags::IsWorking) {
            let last_non_user = self
                .chat
                .iter()
                .rposition(|m| !matches!(m, Message::User(_)));
            match last_non_user {
                Some(i) if matches!(self.chat[i], Message::Assistant(ref m) if m.is_streaming()) => {
                    // Streaming assistant separates dispatched from queued
                    let first_trailing = i + 1;
                    if first_trailing < self.chat.len() {
                        Some(first_trailing)
                    } else {
                        None
                    }
                }
                Some(i) => {
                    // No streaming assistant yet — skip past the dispatched
                    // user messages (1 for single dispatch, N for batch)
                    let first_trailing = i + 1;
                    let skip = self.dispatch_state.dispatched_count().max(1);
                    let queued_start = first_trailing + skip;
                    if queued_start < self.chat.len() {
                        Some(queued_start)
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else {
            None
        };

        for (i, message) in self.chat.iter().enumerate() {
            match message {
                Message::Error(err) => {
                    self.error_chat(ctx.i18n, err, ui);
                }
                Message::User(msg) => {
                    let is_queued = queued_from.is_some_and(|qi| i >= qi);
                    self.user_chat(msg, is_queued, ui);
                }
                Message::Assistant(msg) => {
                    self.assistant_chat(msg, ui);
                }
                Message::ToolResponse(msg) => {
                    Self::tool_response_ui(msg, is_agentic, ui);
                }
                Message::System(_msg) => {
                    // system prompt is not rendered. Maybe we could
                    // have a debug option to show this
                }
                Message::ToolCalls(toolcalls) => {
                    if let Some(note_action) = Self::tool_calls_ui(ctx, toolcalls, ui) {
                        response = DaveResponse::note(note_action);
                    }
                }
                Message::PermissionRequest(request) => {
                    // Permission requests only in Agentic mode
                    if is_agentic {
                        if let Some(action) = self.permission_request_ui(request, ui) {
                            response = DaveResponse::new(action);
                        }
                    }
                }
                Message::CompactionComplete(info) => {
                    // Compaction only in Agentic mode
                    if is_agentic {
                        Self::compaction_complete_ui(info, ui);
                    }
                }
                Message::Subagent(info) => {
                    // Subagents only in Agentic mode
                    if is_agentic {
                        Self::subagent_ui(info, ui);
                    }
                }
            };
        }

        // Show status line at the bottom of chat when working or compacting
        let status_text = if is_agentic && self.flags.contains(DaveUiFlags::IsCompacting) {
            Some("compacting...")
        } else if self.flags.contains(DaveUiFlags::IsWorking) {
            Some("computing...")
        } else {
            None
        };

        if let Some(status) = status_text {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(status)
                        .color(ui.visuals().weak_text_color())
                        .italics(),
                );
                // Don't show interrupt hint for remote sessions
                if !self.flags.contains(DaveUiFlags::IsRemote) {
                    ui.label(
                        egui::RichText::new("(press esc to interrupt)")
                            .color(ui.visuals().weak_text_color())
                            .small(),
                    );
                }
            });
        }

        response
    }

    fn tool_response_ui(tool_response: &ToolResponse, is_agentic: bool, ui: &mut egui::Ui) {
        match tool_response.responses() {
            ToolResponses::ExecutedTool(result) => {
                if is_agentic {
                    Self::executed_tool_ui(result, ui);
                }
            }
            _ => {
                //ui.label(format!("tool_response: {:?}", tool_response));
            }
        }
    }

    /// Render a permission request with Allow/Deny buttons or response state
    fn permission_request_ui(
        &mut self,
        request: &PermissionRequest,
        ui: &mut egui::Ui,
    ) -> Option<DaveAction> {
        let mut action = None;

        let inner_margin = 8.0;
        let corner_radius = 6.0;
        let spacing_x = 8.0;

        ui.spacing_mut().item_spacing.x = spacing_x;

        match request.response {
            Some(PermissionResponseType::Allowed) => {
                // Check if this is an answered AskUserQuestion with stored summary
                if let Some(summary) = &request.answer_summary {
                    super::ask_user_question_summary_ui(summary, ui);
                    return None;
                }

                // Responded state: Allowed (generic fallback)
                egui::Frame::new()
                    .fill(ui.visuals().widgets.noninteractive.bg_fill)
                    .inner_margin(inner_margin)
                    .corner_radius(corner_radius)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Allowed")
                                    .color(egui::Color32::from_rgb(100, 180, 100))
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new(&request.tool_name)
                                    .color(ui.visuals().text_color()),
                            );
                        });
                    });
            }
            Some(PermissionResponseType::Denied) => {
                // Responded state: Denied
                egui::Frame::new()
                    .fill(ui.visuals().widgets.noninteractive.bg_fill)
                    .inner_margin(inner_margin)
                    .corner_radius(corner_radius)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Denied")
                                    .color(egui::Color32::from_rgb(200, 100, 100))
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new(&request.tool_name)
                                    .color(ui.visuals().text_color()),
                            );
                        });
                    });
            }
            None => {
                // Check if this is an ExitPlanMode tool call
                if request.tool_name == "ExitPlanMode" {
                    return self.exit_plan_mode_ui(request, ui);
                }

                // Check if this is an AskUserQuestion tool call
                if request.tool_name == "AskUserQuestion" {
                    if let Ok(questions) =
                        serde_json::from_value::<AskUserQuestionInput>(request.tool_input.clone())
                    {
                        if let (Some(answers_map), Some(index_map)) =
                            (&mut self.question_answers, &mut self.question_index)
                        {
                            return super::ask_user_question_ui(
                                request,
                                &questions,
                                answers_map,
                                index_map,
                                ui,
                            );
                        }
                    }
                }

                // Check if this is a file update (Edit or Write tool)
                if let Some(file_update) =
                    FileUpdate::from_tool_call(&request.tool_name, &request.tool_input)
                {
                    // Render file update with diff view
                    egui::Frame::new()
                        .fill(ui.visuals().widgets.noninteractive.bg_fill)
                        .inner_margin(inner_margin)
                        .corner_radius(corner_radius)
                        .stroke(egui::Stroke::new(1.0, ui.visuals().warn_fg_color))
                        .show(ui, |ui| {
                            // Header with file path
                            diff::file_path_header(&file_update, ui);

                            // Diff view (expand context only for local sessions)
                            let is_local = !self.flags.contains(DaveUiFlags::IsRemote);
                            diff::file_update_ui(&file_update, is_local, ui);

                            // Approve/deny buttons at the bottom left
                            ui.horizontal(|ui| {
                                self.permission_buttons(request, ui, &mut action);
                            });
                        });
                } else {
                    // Parse tool input for display (existing logic)
                    let obj = request.tool_input.as_object();
                    let description = obj
                        .and_then(|o| o.get("description"))
                        .and_then(|v| v.as_str());
                    let command = obj.and_then(|o| o.get("command")).and_then(|v| v.as_str());
                    let single_value = obj
                        .filter(|o| o.len() == 1)
                        .and_then(|o| o.values().next())
                        .and_then(|v| v.as_str());

                    // Pending state: Show Allow/Deny buttons
                    egui::Frame::new()
                        .fill(ui.visuals().widgets.noninteractive.bg_fill)
                        .inner_margin(inner_margin)
                        .corner_radius(corner_radius)
                        .stroke(egui::Stroke::new(1.0, ui.visuals().warn_fg_color))
                        .show(ui, |ui| {
                            // Tool info display
                            if let Some(desc) = description {
                                // Format: ToolName: description
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&request.tool_name).strong());
                                    ui.label(desc);
                                });
                                // Command on next line if present
                                if let Some(cmd) = command {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(cmd).monospace())
                                            .wrap_mode(egui::TextWrapMode::Wrap),
                                    );
                                }
                            } else if let Some(value) = single_value {
                                // Format: ToolName `value`
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&request.tool_name).strong());
                                    ui.label(egui::RichText::new(value).monospace());
                                });
                            } else {
                                // Fallback: show JSON
                                ui.label(egui::RichText::new(&request.tool_name).strong());
                                let formatted = serde_json::to_string_pretty(&request.tool_input)
                                    .unwrap_or_else(|_| request.tool_input.to_string());
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(formatted).monospace().size(11.0),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            }

                            // Buttons on their own line
                            ui.horizontal(|ui| {
                                self.permission_buttons(request, ui, &mut action);
                            });
                        });
                }
            }
        }

        action
    }

    /// Render Allow/Deny buttons aligned to the right with keybinding hints
    fn permission_buttons(
        &self,
        request: &PermissionRequest,
        ui: &mut egui::Ui,
        action: &mut Option<DaveAction>,
    ) {
        let shift_held = ui.input(|i| i.modifiers.shift);
        let in_tentative = self.permission_message_state != PermissionMessageState::None;

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            if in_tentative {
                tentative_send_ui(self.permission_message_state, "Allow", "Deny", ui, action);
            } else {
                let button_text_color = ui.visuals().widgets.active.fg_stroke.color;

                // Allow button (green) with integrated keybind hint
                let allow_response = super::badge::ActionButton::new(
                    "Allow",
                    egui::Color32::from_rgb(34, 139, 34),
                    button_text_color,
                )
                .keybind("1")
                .show(ui)
                .on_hover_text("Press 1 to allow, Shift+1 to allow with message");

                // Deny button (red) with integrated keybind hint
                let deny_response = super::badge::ActionButton::new(
                    "Deny",
                    egui::Color32::from_rgb(178, 34, 34),
                    button_text_color,
                )
                .keybind("2")
                .show(ui)
                .on_hover_text("Press 2 to deny, Shift+2 to deny with message");

                if deny_response.clicked() {
                    if shift_held {
                        *action = Some(DaveAction::TentativeDeny);
                    } else {
                        *action = Some(DaveAction::PermissionResponse {
                            request_id: request.id,
                            response: PermissionResponse::Deny {
                                reason: "User denied".into(),
                            },
                        });
                    }
                }

                if allow_response.clicked() {
                    if shift_held {
                        *action = Some(DaveAction::TentativeAccept);
                    } else {
                        *action = Some(DaveAction::PermissionResponse {
                            request_id: request.id,
                            response: PermissionResponse::Allow { message: None },
                        });
                    }
                }

                add_msg_link(ui, shift_held, action);
            }
        });
    }

    /// Render ExitPlanMode tool call with Approve/Reject buttons
    fn exit_plan_mode_ui(
        &self,
        request: &PermissionRequest,
        ui: &mut egui::Ui,
    ) -> Option<DaveAction> {
        let mut action = None;
        let inner_margin = 12.0;
        let corner_radius = 8.0;

        egui::Frame::new()
            .fill(ui.visuals().widgets.noninteractive.bg_fill)
            .inner_margin(inner_margin)
            .corner_radius(corner_radius)
            .stroke(egui::Stroke::new(1.0, ui.visuals().selection.stroke.color))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    // Header with badge
                    ui.horizontal(|ui| {
                        super::badge::StatusBadge::new("PLAN")
                            .variant(super::badge::BadgeVariant::Info)
                            .show(ui);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Plan ready for approval").strong());
                    });

                    ui.add_space(8.0);

                    // Render plan content as markdown (pre-parsed at construction)
                    if let Some(plan) = &request.cached_plan {
                        markdown_ui::render_assistant_message(
                            &plan.elements,
                            None,
                            &plan.source,
                            ui,
                        );
                    } else if let Some(plan_text) =
                        request.tool_input.get("plan").and_then(|v| v.as_str())
                    {
                        // Fallback: render as plain text
                        ui.label(plan_text);
                    }

                    ui.add_space(8.0);

                    // Approve/Reject buttons with shift support for adding message
                    let shift_held = ui.input(|i| i.modifiers.shift);
                    let in_tentative =
                        self.permission_message_state != PermissionMessageState::None;

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if in_tentative {
                            tentative_send_ui(
                                self.permission_message_state,
                                "Approve",
                                "Reject",
                                ui,
                                &mut action,
                            );
                        } else {
                            let button_text_color = ui.visuals().widgets.active.fg_stroke.color;

                            // Approve button (green)
                            let approve_response = super::badge::ActionButton::new(
                                "Approve",
                                egui::Color32::from_rgb(34, 139, 34),
                                button_text_color,
                            )
                            .keybind("1")
                            .show(ui)
                            .on_hover_text("Press 1 to approve, Shift+1 to approve with message");

                            if approve_response.clicked() {
                                if shift_held {
                                    action = Some(DaveAction::TentativeAccept);
                                } else {
                                    action = Some(DaveAction::ExitPlanMode {
                                        request_id: request.id,
                                        approved: true,
                                    });
                                }
                            }

                            // Compact & Approve button (blue, no keybind)
                            let compact_response = super::badge::ActionButton::new(
                                "Compact & Approve",
                                egui::Color32::from_rgb(59, 130, 246),
                                button_text_color,
                            )
                            .show(ui)
                            .on_hover_text("Compact context then start implementing");

                            if compact_response.clicked() {
                                action = Some(DaveAction::CompactAndApprove {
                                    request_id: request.id,
                                });
                            }

                            // Reject button (red)
                            let reject_response = super::badge::ActionButton::new(
                                "Reject",
                                egui::Color32::from_rgb(178, 34, 34),
                                button_text_color,
                            )
                            .keybind("2")
                            .show(ui)
                            .on_hover_text("Press 2 to reject, Shift+2 to reject with message");

                            if reject_response.clicked() {
                                if shift_held {
                                    action = Some(DaveAction::TentativeDeny);
                                } else {
                                    action = Some(DaveAction::ExitPlanMode {
                                        request_id: request.id,
                                        approved: false,
                                    });
                                }
                            }

                            add_msg_link(ui, shift_held, &mut action);
                        }
                    });
                });
            });

        action
    }

    /// Render tool result metadata as a compact line
    fn executed_tool_ui(result: &ExecutedTool, ui: &mut egui::Ui) {
        if let Some(file_update) = &result.file_update {
            // File edit with diff — show collapsible header with inline diff
            let expand_id = ui.id().with("exec_diff").with(&result.summary);
            let is_small = file_update.diff_lines().len() < 10;
            let expanded: bool = ui.data(|d| d.get_temp(expand_id).unwrap_or(is_small));

            let header_resp = ui
                .horizontal(|ui| {
                    let arrow = if expanded { "▼" } else { "▶" };
                    ui.add(egui::Label::new(
                        egui::RichText::new(arrow)
                            .size(10.0)
                            .color(ui.visuals().text_color().gamma_multiply(0.5)),
                    ));
                    ui.add(egui::Label::new(
                        egui::RichText::new(&result.tool_name)
                            .size(11.0)
                            .color(ui.visuals().text_color().gamma_multiply(0.6))
                            .monospace(),
                    ));
                    if !result.summary.is_empty() {
                        ui.add(egui::Label::new(
                            egui::RichText::new(&result.summary)
                                .size(11.0)
                                .color(ui.visuals().text_color().gamma_multiply(0.4))
                                .monospace(),
                        ));
                    }
                })
                .response
                .interact(egui::Sense::click());

            if header_resp.clicked() {
                ui.data_mut(|d| d.insert_temp(expand_id, !expanded));
            }

            if expanded {
                diff::file_path_header(file_update, ui);
                diff::file_update_ui(file_update, false, ui);
            }
        } else {
            // Compact single-line display with subdued styling
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new(&result.tool_name)
                        .size(11.0)
                        .color(ui.visuals().text_color().gamma_multiply(0.6))
                        .monospace(),
                ));
                if !result.summary.is_empty() {
                    ui.add(egui::Label::new(
                        egui::RichText::new(&result.summary)
                            .size(11.0)
                            .color(ui.visuals().text_color().gamma_multiply(0.4))
                            .monospace(),
                    ));
                }
            });
        }
    }

    /// Render compaction complete notification
    fn compaction_complete_ui(info: &CompactionInfo, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(
                egui::RichText::new("✓")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(100, 180, 100)),
            ));
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Compacted ({} tokens)", info.pre_tokens))
                    .size(11.0)
                    .color(ui.visuals().weak_text_color())
                    .italics(),
            ));
        });
    }

    /// Render a single subagent's status with expandable tool results
    fn subagent_ui(info: &SubagentInfo, ui: &mut egui::Ui) {
        let tool_count = info.tool_results.len();
        let has_tools = tool_count > 0;
        // Compute expand ID from outer ui, before horizontal changes the id scope
        let expand_id = ui.id().with("subagent_expand").with(&info.task_id);

        ui.horizontal(|ui| {
            // Status badge with color based on status
            let variant = match info.status {
                SubagentStatus::Running => BadgeVariant::Warning,
                SubagentStatus::Completed => BadgeVariant::Success,
                SubagentStatus::Failed => BadgeVariant::Destructive,
            };
            StatusBadge::new(&info.subagent_type)
                .variant(variant)
                .show(ui);

            // Description
            ui.label(
                egui::RichText::new(&info.description)
                    .size(11.0)
                    .color(ui.visuals().text_color().gamma_multiply(0.7)),
            );

            // Show spinner for running subagents
            if info.status == SubagentStatus::Running {
                ui.add(egui::Spinner::new().size(11.0));
            }

            // Tool count indicator (clickable to expand)
            if has_tools {
                let expanded = ui.data(|d| d.get_temp::<bool>(expand_id).unwrap_or(false));
                let arrow = if expanded { "▾" } else { "▸" };
                let label = format!("{} ({} tools)", arrow, tool_count);
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(label)
                                .size(10.0)
                                .color(ui.visuals().text_color().gamma_multiply(0.4)),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .clicked()
                {
                    ui.data_mut(|d| d.insert_temp(expand_id, !expanded));
                }
            }
        });

        // Expanded tool results
        if has_tools {
            let expanded = ui.data(|d| d.get_temp::<bool>(expand_id).unwrap_or(false));
            if expanded {
                ui.indent(("subagent_tools", &info.task_id), |ui| {
                    for result in &info.tool_results {
                        Self::executed_tool_ui(result, ui);
                    }
                });
            }
        }
    }

    fn search_call_ui(
        ctx: &mut AppContext,
        query_call: &crate::tools::QueryCall,
        ui: &mut egui::Ui,
    ) {
        ui.add(search_icon(16.0, 16.0));
        ui.add_space(8.0);

        query_call_ui(
            ctx.img_cache,
            ctx.ndb,
            query_call,
            ctx.media_jobs.sender(),
            ui,
        );
    }

    /// The ai has asked us to render some notes, so we do that here
    fn present_notes_ui(
        ctx: &mut AppContext,
        call: &PresentNotesCall,
        ui: &mut egui::Ui,
    ) -> Option<NoteAction> {
        let mut note_context = NoteContext {
            ndb: ctx.ndb,
            accounts: ctx.accounts,
            img_cache: ctx.img_cache,
            note_cache: ctx.note_cache,
            zaps: ctx.zaps,
            pool: ctx.pool,
            jobs: ctx.media_jobs.sender(),
            unknown_ids: ctx.unknown_ids,
            nip05_cache: ctx.nip05_cache,
            clipboard: ctx.clipboard,
            i18n: ctx.i18n,
            global_wallet: ctx.global_wallet,
        };

        let txn = Transaction::new(note_context.ndb).unwrap();

        egui::ScrollArea::horizontal()
            .max_height(400.0)
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let mut action: Option<NoteAction> = None;

                    for note_id in &call.note_ids {
                        let Ok(note) = note_context.ndb.get_note_by_id(&txn, note_id.bytes())
                        else {
                            continue;
                        };

                        let r = ui
                            .allocate_ui_with_layout(
                                [400.0, 400.0].into(),
                                Layout::centered_and_justified(ui.layout().main_dir()),
                                |ui| {
                                    notedeck_ui::NoteView::new(
                                        &mut note_context,
                                        &note,
                                        NoteOptions::default(),
                                    )
                                    .preview_style()
                                    .hide_media(true)
                                    .show(ui)
                                },
                            )
                            .inner;

                        if r.action.is_some() {
                            action = r.action;
                        }
                    }

                    action
                })
                .inner
            })
            .inner
    }

    fn tool_calls_ui(
        ctx: &mut AppContext,
        toolcalls: &[ToolCall],
        ui: &mut egui::Ui,
    ) -> Option<NoteAction> {
        let mut note_action: Option<NoteAction> = None;

        ui.vertical(|ui| {
            for call in toolcalls {
                match call.calls() {
                    ToolCalls::PresentNotes(call) => {
                        let r = Self::present_notes_ui(ctx, call, ui);
                        if r.is_some() {
                            note_action = r;
                        }
                    }
                    ToolCalls::Invalid(err) => {
                        ui.label(format!("invalid tool call: {err:?}"));
                    }
                    ToolCalls::Query(search_call) => {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_size().x, 32.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                Self::search_call_ui(ctx, search_call, ui);
                            },
                        );
                    }
                }
            }
        });

        note_action
    }

    fn inputbox(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let i18n = &mut *app_ctx.i18n;
        //ui.add_space(Self::chat_margin(ui.ctx()) as f32);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                let mut dave_response = DaveResponse::none();

                // Always show Ask button (messages queue while working)
                if ui
                    .add(
                        egui::Button::new(tr!(
                            i18n,
                            "Ask",
                            "Button to send message to Dave AI assistant"
                        ))
                        .min_size(egui::vec2(60.0, 44.0)),
                    )
                    .clicked()
                {
                    dave_response = DaveResponse::send();
                }

                // Show Stop button alongside Ask for local working sessions
                if self.flags.contains(DaveUiFlags::IsWorking)
                    && !self.flags.contains(DaveUiFlags::IsRemote)
                {
                    if ui
                        .add(
                            egui::Button::new(tr!(
                                i18n,
                                "Stop",
                                "Button to interrupt/stop the AI operation"
                            ))
                            .min_size(egui::vec2(60.0, 44.0)),
                        )
                        .clicked()
                    {
                        dave_response = DaveResponse::new(DaveAction::Interrupt);
                    }

                    // Show "Press Esc again" indicator when interrupt is pending
                    if self.flags.contains(DaveUiFlags::InterruptPending) {
                        ui.label(
                            egui::RichText::new("Press Esc again to stop")
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                }

                let r = ui.add(
                    egui::TextEdit::multiline(self.input)
                        .desired_width(f32::INFINITY)
                        .return_key(KeyboardShortcut::new(
                            Modifiers {
                                shift: true,
                                ..Default::default()
                            },
                            Key::Enter,
                        ))
                        .hint_text(
                            egui::RichText::new(tr!(
                                i18n,
                                "Ask dave anything...",
                                "Placeholder text for Dave AI input field"
                            ))
                            .weak(),
                        )
                        .frame(false),
                );
                notedeck_ui::context_menu::input_context(
                    ui,
                    &r,
                    app_ctx.clipboard,
                    self.input,
                    notedeck_ui::context_menu::PasteBehavior::Append,
                );

                // Request focus if flagged (e.g., after spawning a new agent or entering tentative state)
                if *self.focus_requested {
                    r.request_focus();
                    *self.focus_requested = false;
                }

                // Unfocus text input when there's a pending permission request
                // UNLESS we're in tentative state (user needs to type message)
                let in_tentative_state =
                    self.permission_message_state != PermissionMessageState::None;
                if self.flags.contains(DaveUiFlags::HasPendingPerm) && !in_tentative_state {
                    r.surrender_focus();
                }

                if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    DaveResponse::send()
                } else {
                    dave_response
                }
            })
            .inner
        })
        .inner
    }

    fn user_chat(&self, msg: &str, is_queued: bool, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            let r = egui::Frame::new()
                .inner_margin(10.0)
                .corner_radius(10.0)
                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(msg)
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .selectable(true),
                    );
                    if is_queued {
                        ui.label(
                            egui::RichText::new("queued")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            r.response.context_menu(|ui| {
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(msg.to_owned());
                    ui.close_menu();
                }
            });
        });
    }

    fn assistant_chat(&self, msg: &AssistantMessage, ui: &mut egui::Ui) {
        let elements = msg.parsed_elements();
        let partial = msg.partial();
        let buffer = msg.buffer();
        let text = msg.text().to_owned();
        let r = ui.scope(|ui| {
            markdown_ui::render_assistant_message(elements, partial, buffer, ui);
        });
        r.response.context_menu(|ui| {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(text.clone());
                ui.close_menu();
            }
        });
    }
}

/// Send button + clickable accept/deny toggle shown when in tentative state.
fn tentative_send_ui(
    state: PermissionMessageState,
    accept_label: &str,
    deny_label: &str,
    ui: &mut egui::Ui,
    action: &mut Option<DaveAction>,
) {
    if ui
        .add(egui::Button::new(egui::RichText::new("Send").strong()))
        .clicked()
    {
        *action = Some(DaveAction::Send);
    }

    match state {
        PermissionMessageState::TentativeAccept => {
            if ui
                .link(
                    egui::RichText::new(format!("✓ Will {accept_label}"))
                        .color(egui::Color32::from_rgb(100, 180, 100))
                        .strong(),
                )
                .clicked()
            {
                *action = Some(DaveAction::TentativeDeny);
            }
        }
        PermissionMessageState::TentativeDeny => {
            if ui
                .link(
                    egui::RichText::new(format!("✗ Will {deny_label}"))
                        .color(egui::Color32::from_rgb(200, 100, 100))
                        .strong(),
                )
                .clicked()
            {
                *action = Some(DaveAction::TentativeAccept);
            }
        }
        PermissionMessageState::None => {}
    }
}

/// Clickable "+ msg [⇧]" link that enters tentative accept mode.
/// Highlights in warn color when Shift is held on desktop.
fn add_msg_link(ui: &mut egui::Ui, shift_held: bool, action: &mut Option<DaveAction>) {
    let color = if shift_held {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().weak_text_color()
    };
    if ui
        .link(egui::RichText::new("+ msg [⇧]").color(color).small())
        .clicked()
    {
        *action = Some(DaveAction::TentativeAccept);
    }
}

/// Renders the status bar containing git status and toggle badges.
fn status_bar_ui(
    mut git_status: Option<&mut GitStatusCache>,
    is_agentic: bool,
    plan_mode_active: bool,
    auto_steal_focus: bool,
    usage: Option<&crate::messages::UsageInfo>,
    context_window: u64,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let snapshot = git_status
        .as_deref()
        .and_then(git_status_ui::StatusSnapshot::from_cache);

    ui.vertical(|ui| {
        let action = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;

                if let Some(git_status) = git_status.as_deref_mut() {
                    git_status_ui::git_status_content_ui(git_status, &snapshot, ui);

                    // Right-aligned section: usage bar, badges, then refresh
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let badge_action = if is_agentic {
                            toggle_badges_ui(ui, plan_mode_active, auto_steal_focus)
                        } else {
                            None
                        };
                        if is_agentic {
                            usage_bar_ui(usage, context_window, ui);
                        }
                        badge_action
                    })
                    .inner
                } else if is_agentic {
                    // No git status (remote session) - just show badges and usage
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let badge_action = toggle_badges_ui(ui, plan_mode_active, auto_steal_focus);
                        usage_bar_ui(usage, context_window, ui);
                        badge_action
                    })
                    .inner
                } else {
                    None
                }
            })
            .inner;

        if let Some(git_status) = git_status.as_deref() {
            git_status_ui::git_expanded_files_ui(git_status, &snapshot, ui);
        }

        action
    })
    .inner
}

/// Format a token count in a compact human-readable form (e.g. "45K", "1.2M")
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Renders the usage fill bar showing context window consumption.
fn usage_bar_ui(
    usage: Option<&crate::messages::UsageInfo>,
    context_window: u64,
    ui: &mut egui::Ui,
) {
    let total = usage.map(|u| u.total_tokens()).unwrap_or(0);
    if total == 0 {
        return;
    }
    let usage = usage.unwrap();
    let fraction = (total as f64 / context_window as f64).min(1.0) as f32;

    // Color based on fill level: green → yellow → red
    let bar_color = if fraction < 0.5 {
        egui::Color32::from_rgb(100, 180, 100)
    } else if fraction < 0.8 {
        egui::Color32::from_rgb(200, 180, 60)
    } else {
        egui::Color32::from_rgb(200, 80, 80)
    };

    let weak = ui.visuals().weak_text_color();

    // Cost label
    if let Some(cost) = usage.cost_usd {
        if cost > 0.0 {
            ui.add(egui::Label::new(
                egui::RichText::new(format!("${:.2}", cost))
                    .size(10.0)
                    .color(weak),
            ));
        }
    }

    // Token count label
    ui.add(egui::Label::new(
        egui::RichText::new(format!(
            "{} / {}",
            format_tokens(total),
            format_tokens(context_window)
        ))
        .size(10.0)
        .color(weak),
    ));

    // Fill bar
    let bar_width = 60.0;
    let bar_height = 8.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);

    // Fill
    let fill_rect =
        egui::Rect::from_min_size(rect.min, egui::vec2(bar_width * fraction, bar_height));
    painter.rect_filled(fill_rect, 3.0, bar_color);
}

/// Render clickable PLAN and AUTO toggle badges. Returns an action if clicked.
fn toggle_badges_ui(
    ui: &mut egui::Ui,
    plan_mode_active: bool,
    auto_steal_focus: bool,
) -> Option<DaveAction> {
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut action = None;

    // AUTO badge (rendered first in right-to-left, so it appears rightmost)
    let mut auto_badge = super::badge::StatusBadge::new("AUTO").variant(if auto_steal_focus {
        super::badge::BadgeVariant::Info
    } else {
        super::badge::BadgeVariant::Default
    });
    if ctrl_held {
        auto_badge = auto_badge.keybind("\\");
    }
    if auto_badge
        .show(ui)
        .on_hover_text("Click or Ctrl+\\ to toggle auto-focus mode")
        .clicked()
    {
        action = Some(DaveAction::ToggleAutoSteal);
    }

    // PLAN badge
    let mut plan_badge = super::badge::StatusBadge::new("PLAN").variant(if plan_mode_active {
        super::badge::BadgeVariant::Info
    } else {
        super::badge::BadgeVariant::Default
    });
    if ctrl_held {
        plan_badge = plan_badge.keybind("M");
    }
    if plan_badge
        .show(ui)
        .on_hover_text("Click or Ctrl+M to toggle plan mode")
        .clicked()
    {
        action = Some(DaveAction::TogglePlanMode);
    }

    action
}

fn session_header_ui(ui: &mut egui::Ui, details: &SessionDetails, backend_type: BackendType) {
    ui.horizontal(|ui| {
        // Backend icon
        if backend_type.is_agentic() {
            let icon = crate::ui::backend_icon(backend_type).max_height(16.0);
            ui.add(icon);
        }

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add(
                egui::Label::new(egui::RichText::new(details.display_title()).size(13.0))
                    .wrap_mode(egui::TextWrapMode::Truncate),
            );
            if let Some(cwd) = &details.cwd {
                let cwd_display = if details.home_dir.is_empty() {
                    crate::path_utils::abbreviate_path(cwd)
                } else {
                    crate::path_utils::abbreviate_with_home(cwd, &details.home_dir)
                };
                let display_text = if details.hostname.is_empty() {
                    cwd_display
                } else {
                    format!("{}:{}", details.hostname, cwd_display)
                };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(display_text)
                            .monospace()
                            .size(10.0)
                            .weak(),
                    )
                    .wrap_mode(egui::TextWrapMode::Truncate),
                );
            }
        });
    });
}
