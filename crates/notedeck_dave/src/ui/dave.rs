use super::badge::{BadgeVariant, StatusBadge};
use super::diff;
use super::git_status_ui;
use super::markdown_ui;
use super::query_ui::query_call_ui;
use super::top_buttons::top_buttons_ui;
use crate::{
    config::{AiMode, DaveSettings},
    file_update::FileUpdate,
    git_status::GitStatusCache,
    messages::{
        AskUserQuestionInput, AssistantMessage, CompactionInfo, Message, PermissionRequest,
        PermissionResponse, PermissionResponseType, QuestionAnswer, SubagentInfo, SubagentStatus,
        ToolResult,
    },
    session::PermissionMessageState,
    tools::{PresentNotesCall, ToolCall, ToolCalls, ToolResponse},
};
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::Transaction;
use notedeck::{tr, AppContext, Localization, NoteAction, NoteContext};
use notedeck_ui::{icons::search_icon, NoteOptions};
use std::collections::HashMap;
use uuid::Uuid;

/// DaveUi holds all of the data it needs to render itself
pub struct DaveUi<'a> {
    chat: &'a [Message],
    trial: bool,
    input: &'a mut String,
    compact: bool,
    is_working: bool,
    interrupt_pending: bool,
    has_pending_permission: bool,
    focus_requested: &'a mut bool,
    plan_mode_active: bool,
    /// State for tentative permission response (waiting for message)
    permission_message_state: PermissionMessageState,
    /// State for AskUserQuestion responses (selected options per question)
    question_answers: Option<&'a mut HashMap<Uuid, Vec<QuestionAnswer>>>,
    /// Current question index for multi-question AskUserQuestion
    question_index: Option<&'a mut HashMap<Uuid, usize>>,
    /// Whether conversation compaction is in progress
    is_compacting: bool,
    /// Whether auto-steal focus mode is active
    auto_steal_focus: bool,
    /// AI interaction mode (Chat vs Agentic)
    ai_mode: AiMode,
    /// Git status cache for current session (agentic only)
    git_status: Option<&'a mut GitStatusCache>,
    /// Whether this is a remote session (no local Claude process)
    is_remote: bool,
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
}

impl<'a> DaveUi<'a> {
    pub fn new(
        trial: bool,
        chat: &'a [Message],
        input: &'a mut String,
        focus_requested: &'a mut bool,
        ai_mode: AiMode,
    ) -> Self {
        DaveUi {
            trial,
            chat,
            input,
            compact: false,
            is_working: false,
            interrupt_pending: false,
            has_pending_permission: false,
            focus_requested,
            plan_mode_active: false,
            permission_message_state: PermissionMessageState::None,
            question_answers: None,
            question_index: None,
            is_compacting: false,
            auto_steal_focus: false,
            ai_mode,
            git_status: None,
            is_remote: false,
        }
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

    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    pub fn is_working(mut self, is_working: bool) -> Self {
        self.is_working = is_working;
        self
    }

    pub fn interrupt_pending(mut self, interrupt_pending: bool) -> Self {
        self.interrupt_pending = interrupt_pending;
        self
    }

    pub fn has_pending_permission(mut self, has_pending_permission: bool) -> Self {
        self.has_pending_permission = has_pending_permission;
        self
    }

    pub fn plan_mode_active(mut self, plan_mode_active: bool) -> Self {
        self.plan_mode_active = plan_mode_active;
        self
    }

    pub fn is_compacting(mut self, is_compacting: bool) -> Self {
        self.is_compacting = is_compacting;
        self
    }

    /// Set the git status cache. Mutable because the UI toggles
    /// expand/collapse and triggers refresh on button click.
    pub fn git_status(mut self, cache: &'a mut GitStatusCache) -> Self {
        self.git_status = Some(cache);
        self
    }

    pub fn auto_steal_focus(mut self, auto_steal_focus: bool) -> Self {
        self.auto_steal_focus = auto_steal_focus;
        self
    }

    pub fn is_remote(mut self, is_remote: bool) -> Self {
        self.is_remote = is_remote;
        self
    }

    fn chat_margin(&self, ctx: &egui::Context) -> i8 {
        if self.compact || notedeck::ui::is_narrow(ctx) {
            20
        } else {
            100
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
        // Skip top buttons in compact mode (scene panel has its own controls)
        let action = if self.compact {
            None
        } else {
            top_buttons_ui(app_ctx, ui)
        };

        egui::Frame::NONE
            .show(ui, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let margin = self.chat_margin(ui.ctx());
                    let bottom_margin = 100;

                    let r = egui::Frame::new()
                        .outer_margin(egui::Margin {
                            left: margin,
                            right: margin,
                            top: 0,
                            bottom: bottom_margin,
                        })
                        .inner_margin(egui::Margin::same(8))
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(12.0)
                        .show(ui, |ui| self.inputbox(app_ctx.i18n, ui))
                        .inner;

                    if let Some(git_status) = &mut self.git_status {
                        // Explicitly reserve height so bottom_up layout
                        // keeps the chat ScrollArea from overlapping.
                        let h = if git_status.expanded { 200.0 } else { 24.0 };
                        let w = ui.available_width();
                        ui.allocate_ui(egui::vec2(w, h), |ui| {
                            egui::Frame::new()
                                .outer_margin(egui::Margin {
                                    left: margin,
                                    right: margin,
                                    top: 4,
                                    bottom: 0,
                                })
                                .show(ui, |ui| {
                                    git_status_ui::git_status_bar_ui(git_status, ui);
                                });
                        });
                    }

                    let chat_response = egui::ScrollArea::vertical()
                        .id_salt("dave_chat_scroll")
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
        if self.trial {
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

        for message in self.chat {
            match message {
                Message::Error(err) => {
                    self.error_chat(ctx.i18n, err, ui);
                }
                Message::User(msg) => {
                    self.user_chat(msg, ui);
                }
                Message::Assistant(msg) => {
                    self.assistant_chat(msg, ui);
                }
                Message::ToolResponse(msg) => {
                    Self::tool_response_ui(msg, ui);
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
                Message::ToolResult(result) => {
                    // Tool results only in Agentic mode
                    if is_agentic {
                        Self::tool_result_ui(result, ui);
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
        let status_text = if is_agentic && self.is_compacting {
            Some("compacting...")
        } else if self.is_working {
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
                ui.label(
                    egui::RichText::new("(press esc to interrupt)")
                        .color(ui.visuals().weak_text_color())
                        .small(),
                );
            });
        }

        response
    }

    fn tool_response_ui(_tool_response: &ToolResponse, _ui: &mut egui::Ui) {
        //ui.label(format!("tool_response: {:?}", tool_response));
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

                            // Diff view
                            diff::file_update_ui(&file_update, ui);

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

                                    self.permission_buttons(request, ui, &mut action);
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

                                    self.permission_buttons(request, ui, &mut action);
                                });
                            } else {
                                // Fallback: show JSON
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&request.tool_name).strong());

                                    self.permission_buttons(request, ui, &mut action);
                                });
                                let formatted = serde_json::to_string_pretty(&request.tool_input)
                                    .unwrap_or_else(|_| request.tool_input.to_string());
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(formatted).monospace().size(11.0),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            }
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

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let button_text_color = ui.visuals().widgets.active.fg_stroke.color;

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
                    // Shift+click: enter tentative deny mode
                    *action = Some(DaveAction::TentativeDeny);
                } else {
                    // Normal click: immediate deny
                    *action = Some(DaveAction::PermissionResponse {
                        request_id: request.id,
                        response: PermissionResponse::Deny {
                            reason: "User denied".into(),
                        },
                    });
                }
            }

            // Allow button (green) with integrated keybind hint
            let allow_response = super::badge::ActionButton::new(
                "Allow",
                egui::Color32::from_rgb(34, 139, 34),
                button_text_color,
            )
            .keybind("1")
            .show(ui)
            .on_hover_text("Press 1 to allow, Shift+1 to allow with message");

            if allow_response.clicked() {
                if shift_held {
                    // Shift+click: enter tentative accept mode
                    *action = Some(DaveAction::TentativeAccept);
                } else {
                    // Normal click: immediate allow
                    *action = Some(DaveAction::PermissionResponse {
                        request_id: request.id,
                        response: PermissionResponse::Allow { message: None },
                    });
                }
            }

            // Show tentative state indicator OR shift hint
            match self.permission_message_state {
                PermissionMessageState::TentativeAccept => {
                    ui.label(
                        egui::RichText::new("✓ Will Allow")
                            .color(egui::Color32::from_rgb(100, 180, 100))
                            .strong(),
                    );
                }
                PermissionMessageState::TentativeDeny => {
                    ui.label(
                        egui::RichText::new("✗ Will Deny")
                            .color(egui::Color32::from_rgb(200, 100, 100))
                            .strong(),
                    );
                }
                PermissionMessageState::None => {
                    // Always show hint for adding message
                    let hint_color = if shift_held {
                        ui.visuals().warn_fg_color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.label(
                        egui::RichText::new("(⇧ for message)")
                            .color(hint_color)
                            .small(),
                    );
                }
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

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button_text_color = ui.visuals().widgets.active.fg_stroke.color;

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

                        // Show tentative state indicator OR shift hint
                        match self.permission_message_state {
                            PermissionMessageState::TentativeAccept => {
                                ui.label(
                                    egui::RichText::new("✓ Will Approve")
                                        .color(egui::Color32::from_rgb(100, 180, 100))
                                        .strong(),
                                );
                            }
                            PermissionMessageState::TentativeDeny => {
                                ui.label(
                                    egui::RichText::new("✗ Will Reject")
                                        .color(egui::Color32::from_rgb(200, 100, 100))
                                        .strong(),
                                );
                            }
                            PermissionMessageState::None => {
                                let hint_color = if shift_held {
                                    ui.visuals().warn_fg_color
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                ui.label(
                                    egui::RichText::new("(⇧ for message)")
                                        .color(hint_color)
                                        .small(),
                                );
                            }
                        }
                    });
                });
            });

        action
    }

    /// Render tool result metadata as a compact line
    fn tool_result_ui(result: &ToolResult, ui: &mut egui::Ui) {
        // Compact single-line display with subdued styling
        ui.horizontal(|ui| {
            // Tool name in slightly brighter text
            ui.add(egui::Label::new(
                egui::RichText::new(&result.tool_name)
                    .size(11.0)
                    .color(ui.visuals().text_color().gamma_multiply(0.6))
                    .monospace(),
            ));
            // Summary in more subdued text
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

    /// Render a single subagent's status
    fn subagent_ui(info: &SubagentInfo, ui: &mut egui::Ui) {
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
        });
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

    fn inputbox(&mut self, i18n: &mut Localization, ui: &mut egui::Ui) -> DaveResponse {
        //ui.add_space(Self::chat_margin(ui.ctx()) as f32);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                let mut dave_response = DaveResponse::none();

                // Show Stop button when working, Ask button otherwise
                if self.is_working {
                    if ui
                        .add(egui::Button::new(tr!(
                            i18n,
                            "Stop",
                            "Button to interrupt/stop the AI operation"
                        )))
                        .clicked()
                    {
                        dave_response = DaveResponse::new(DaveAction::Interrupt);
                    }

                    // Show "Press Esc again" indicator when interrupt is pending
                    if self.interrupt_pending {
                        ui.label(
                            egui::RichText::new("Press Esc again to stop")
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                } else if ui
                    .add(egui::Button::new(tr!(
                        i18n,
                        "Ask",
                        "Button to send message to Dave AI assistant"
                    )))
                    .clicked()
                {
                    dave_response = DaveResponse::send();
                }

                // Show plan mode and auto-steal indicators only in Agentic mode
                if self.ai_mode == AiMode::Agentic {
                    let ctrl_held = ui.input(|i| i.modifiers.ctrl);

                    // Plan mode indicator with optional keybind hint when Ctrl is held
                    let mut plan_badge =
                        super::badge::StatusBadge::new("PLAN").variant(if self.plan_mode_active {
                            super::badge::BadgeVariant::Info
                        } else {
                            super::badge::BadgeVariant::Default
                        });
                    if ctrl_held {
                        plan_badge = plan_badge.keybind("M");
                    }
                    plan_badge
                        .show(ui)
                        .on_hover_text("Ctrl+M to toggle plan mode");

                    // Auto-steal focus indicator
                    let mut auto_badge =
                        super::badge::StatusBadge::new("AUTO").variant(if self.auto_steal_focus {
                            super::badge::BadgeVariant::Info
                        } else {
                            super::badge::BadgeVariant::Default
                        });
                    if ctrl_held {
                        auto_badge = auto_badge.keybind("\\");
                    }
                    auto_badge
                        .show(ui)
                        .on_hover_text("Ctrl+\\ to toggle auto-focus mode");
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
                notedeck_ui::include_input(ui, &r);

                // Request focus if flagged (e.g., after spawning a new agent or entering tentative state)
                if *self.focus_requested {
                    r.request_focus();
                    *self.focus_requested = false;
                }

                // Unfocus text input when there's a pending permission request
                // UNLESS we're in tentative state (user needs to type message)
                let in_tentative_state =
                    self.permission_message_state != PermissionMessageState::None;
                if self.has_pending_permission && !in_tentative_state {
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

    fn user_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            egui::Frame::new()
                .inner_margin(10.0)
                .corner_radius(10.0)
                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(msg)
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .selectable(true),
                    );
                })
        });
    }

    fn assistant_chat(&self, msg: &AssistantMessage, ui: &mut egui::Ui) {
        let elements = msg.parsed_elements();
        let partial = msg.partial();
        let buffer = msg.buffer();
        markdown_ui::render_assistant_message(elements, partial, buffer, ui);
    }
}
