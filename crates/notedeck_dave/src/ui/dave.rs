use super::badge::{BadgeVariant, StatusBadge};
use super::block_nav::BlockNav;
use super::diff;
use super::git_status_ui;
use super::inspector::{
    inspector_ui, rate_limit_pill_ui, session_tabs_ui, waiting_pill_ui, SessionFacts,
};
use super::keybindings::NormalView;
use super::markdown_ui;
use super::query_ui::query_call_ui;
use super::run_ui;
use super::session_list::{title_with_refs_ui, TitleRefs};
use super::square_loading_spinner::SquareLoadingSpinner;
use super::top_buttons::top_buttons_ui;
use crate::{
    backend::BackendType,
    config::{AiMode, DaveSettings},
    file_update::FileUpdate,
    focus_queue::{FocusPriority, FocusQueueInfo},
    git_status::GitStatusCache,
    inspector::{InspectorSection, InspectorView, SessionInspector, SessionTab},
    messages::{
        ApprovalPromptInput, AssistantMessage, CompactionInfo, ExecutedTool, ImageAttachment,
        Message, PermissionRequest, PermissionResponse, PermissionResponseType, PermissionView,
        QuestionAnswer, RunningTool, SubagentInfo, SubagentStatus, SystemMessage,
        DEFAULT_DENY_REASON,
    },
    session::{PermissionMessageState, SessionDetails, SessionId},
    tools::{PresentNotesCall, ToolCall, ToolCalls, ToolResponse, ToolResponses},
};
use bitflags::bitflags;
use claude_agent_sdk_rs::PermissionMode;
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::Transaction;
use notedeck::{tr, AppContext, Localization, NoteAction, NoteContext};
use notedeck_ui::{icons::search_icon, NoteOptions};
use std::collections::HashMap;
use uuid::Uuid;

/// Wrapped rows a Bash command echo may take before "show all" reveals the
/// rest. A heredoc commit message otherwise fills the row.
const COMMAND_PREVIEW_ROWS: usize = 3;

/// Bytes of a command echo the clamped body lays out each frame. Three rows
/// never need more, and egui copies whatever it lays out.
const COMMAND_PREVIEW_BYTES: usize = 1024;

/// Wrapped rows the head, and separately the tail, of a clamped output preview
/// may take: two per kept line, so one very long line can't undo the clamp.
const OUTPUT_PREVIEW_ROWS: usize = 2 * crate::tool_output::PREVIEW_HEAD_LINES;

/// How much of the viewport's height the "show all" view may fill. It scrolls
/// inside that, so a full build log never runs the length of the chat.
const SHOW_ALL_VIEWPORT_FRACTION: f32 = 0.6;

/// Bytes of a single line the "show all" view lays out. Lines there don't
/// wrap, so this only bounds the cost of a pathological one (minified JSON).
const SHOW_ALL_LINE_BYTES: usize = 2000;

/// Font size of a tool result's body text.
const TOOL_BODY_FONT_SIZE: f32 = 11.0;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DaveUiFlags: u16 {
        const Trial            = 1 << 0;
        const IsWorking        = 1 << 2;
        const HasPendingPerm   = 1 << 4;
        const IsCompacting     = 1 << 5;
        const AutoStealFocus   = 1 << 6;
        const IsRemote         = 1 << 7;
        /// A not-yet-materialized placeholder session (a remote spawn or resume in
        /// flight) — drives the "connecting…" status line + spinner.
        const IsConnecting     = 1 << 8;
        /// A Claude subagent's virtual session: the header says what it
        /// inherits, and its composer sends through its parent.
        const IsSubagent       = 1 << 9;
    }
}

/// DaveUi holds all of the data it needs to render itself
pub struct DaveUi<'a> {
    chat: &'a [Message],
    flags: DaveUiFlags,
    input: &'a mut String,
    focus_requested: &'a mut bool,
    /// Registry of this chat's collapsible blocks, refilled as they render, so a
    /// keybinding can address them next frame.
    nav: &'a mut BlockNav,
    /// Session ID for per-session scroll state
    session_id: SessionId,
    /// State for tentative permission response (waiting for message)
    permission_message_state: PermissionMessageState,
    /// State for shared question-set responses (selected options per question)
    question_answers: Option<&'a mut HashMap<Uuid, Vec<QuestionAnswer>>>,
    /// Current question index for multi-question prompts
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
    /// Why the composer can't send now, if it can't (see
    /// [`DaveUi::composer_block`]).
    composer_block: Option<&'static str>,
    /// Context window size for the current model
    context_window: u64,
    /// The Claude account's rate-limit windows: the account-wide value for a
    /// local session, the session's own for a remote one.
    rate_limit: Option<crate::messages::RateLimitInfo>,
    /// The clock the Details tab reads, in Unix seconds; `None` reads the
    /// system clock. Pinned by snapshot tests so ages and countdowns hold
    /// still.
    now_unix: Option<u64>,
    /// Dispatch lifecycle state, used for queued indicator logic.
    dispatch_state: crate::session::DispatchState,
    /// Whether the current turn has produced content yet, used with
    /// `dispatch_state` for the queued indicator's insert-boundary.
    turn_has_content: bool,
    /// Which backend this session uses
    backend_type: BackendType,
    /// Current permission mode (Default, Plan, AcceptEdits)
    permission_mode: PermissionMode,
    /// When the last activity was seen, as wall-clock unix seconds
    last_activity: Option<u64>,
    /// Focus queue info for mobile NEXT badge
    focus_queue_info: Option<FocusQueueInfo>,
    /// Named run configs for this session's CWD
    run_configs: &'a [crate::config::RunConfig],
    /// IDs of configs currently running for this session
    running_config_ids: Option<&'a std::collections::HashSet<String>>,
    /// Pending image attachments staged for the next send
    pending_images: Option<&'a mut Vec<ImageAttachment>>,
    /// Normal mode and how far into a chord it is, for the which-key strip.
    normal_mode: Option<NormalView>,
    /// The session's inspector state and which tab it shows. `None` draws the
    /// chat with no tab strip.
    inspector: Option<Inspected<'a>>,
    /// The input's slash-command popup; `None` draws no popup.
    slash: Option<&'a mut crate::slash::SlashPopup>,
}

/// The session the Details tab inspects: its inspector and tab state, plus
/// the session state the Overview reads that [`DaveUi`] doesn't already hold.
pub struct Inspected<'a> {
    pub inspector: &'a mut SessionInspector,
    pub view: &'a mut InspectorView,
    pub status: crate::agent_status::AgentStatus,
    /// What will resume the session if it is Waiting.
    pub waiting_on: crate::messages::WaitingOn,
    /// The Claude CLI session id, for `--resume`.
    pub cli_session_id: Option<&'a str>,
    /// The session's allowlist and recorded permission decisions.
    pub permissions: Option<crate::ui::inspector::PermissionFacts<'a>>,
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

/// All actions related to run configurations and process control.
#[derive(Debug)]
pub enum RunAction {
    /// Launch the config with this stable UUID
    Launch { config_id: String },
    /// Stop the running process for the config with this stable UUID
    Stop { config_id: String },
    /// Open the editor to create a new config for the given CWD
    OpenNew { cwd: std::path::PathBuf },
    /// Open the editor to edit an existing config by its stable UUID
    OpenEdit {
        cwd: std::path::PathBuf,
        config_id: String,
    },
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
    /// Exit the tool call: deny it and cancel the current turn
    ExitToolCall {
        request_id: Uuid,
    },
    /// Enter tentative accept mode (Shift+click on Yes)
    TentativeAccept,
    /// Enter tentative deny mode (Shift+click on No)
    TentativeDeny,
    /// Allow always — add to session allowlist and accept
    AllowAlways {
        request_id: Uuid,
    },
    /// Tentative allow always — add to session allowlist, enter message mode
    TentativeAllowAlways,
    /// User responded to a shared question-set prompt
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
    /// Set a specific permission mode (chosen from the mode badge's menu):
    /// Manual / Plan / Accept Edits / Auto.
    SetPermissionMode(PermissionMode),
    /// Toggle auto-steal focus mode (clicked FOCUS badge)
    ToggleAutoSteal,
    /// Trigger manual context compaction
    Compact,
    /// Navigate to the next focus queue item (mobile)
    FocusQueueNext,
    /// All run-config and process-control actions
    Run(RunAction),
    /// Show the Details tab at this section (the tab strip, the section rail,
    /// and deep links from the status bar)
    OpenInspector(InspectorSection),
    /// Show the chat tab again
    ShowChat,
    /// Show the chat with the block cursor on this subagent's row (by its
    /// task id), from the Details tab's Subagents card
    ShowSubagent(String),
    /// Open this file or folder with the system's default app, from the
    /// Details tab's Files card
    OpenPath(std::path::PathBuf),
    /// Show the chat with the block cursor on this chat row's block (by its
    /// index in the chat), from the Details tab's edited files
    ShowChatRow(usize),
    /// Drop an always-allow rule from the active session's runtime
    /// allowlist, from the Details tab's Permissions card
    RevokeAllow(crate::session::RuntimeAllow),
    /// Stop one of the active session's background tasks by its CLI task
    /// id, from the Details tab's Background card once the user confirmed
    StopTask(String),
    /// Switch to the virtual session of the active session's subagent (by
    /// its task id), from its row in the chat
    OpenSubagent(String),
}

impl<'a> DaveUi<'a> {
    pub fn new(
        trial: bool,
        session_id: SessionId,
        chat: &'a [Message],
        input: &'a mut String,
        focus_requested: &'a mut bool,
        nav: &'a mut BlockNav,
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
            nav,
            permission_message_state: PermissionMessageState::None,
            question_answers: None,
            question_index: None,
            ai_mode,
            git_status: None,
            details: None,
            status_dot_color: None,
            composer_block: None,
            context_window: crate::messages::context_window_for_model(None),
            rate_limit: None,
            now_unix: None,
            dispatch_state: crate::session::DispatchState::default(),
            turn_has_content: false,
            backend_type: BackendType::Remote,
            permission_mode: PermissionMode::Default,
            last_activity: None,
            focus_queue_info: None,
            run_configs: &[],
            running_config_ids: None,
            pending_images: None,
            normal_mode: None,
            inspector: None,
            slash: None,
        }
    }

    /// Give the session view its `Chat | Details` tab strip, drawing Details
    /// from `inspected` when its view selects it.
    /// Draw `popup` over the input when a `/` command is typed.
    pub fn slash(mut self, popup: &'a mut crate::slash::SlashPopup) -> Self {
        self.slash = Some(popup);
        self
    }

    pub fn inspector(mut self, inspected: Inspected<'a>) -> Self {
        self.inspector = Some(inspected);
        self
    }

    /// The Claude CLI session id the Details tab's Overview shows. Set after
    /// [`Self::inspector`]; ignored without one.
    pub fn cli_session_id(mut self, id: Option<&'a str>) -> Self {
        if let Some(inspected) = &mut self.inspector {
            inspected.cli_session_id = id;
        }
        self
    }

    /// The permission state the Details tab's Permissions card reads. Set
    /// after [`Self::inspector`]; ignored without one.
    pub fn permission_facts(
        mut self,
        facts: Option<crate::ui::inspector::PermissionFacts<'a>>,
    ) -> Self {
        if let Some(inspected) = &mut self.inspector {
            inspected.permissions = facts;
        }
        self
    }

    /// Whether the session view draws its tab strip: whenever it has an
    /// inspector to draw.
    fn shows_tabs(&self) -> bool {
        self.inspector.is_some()
    }

    /// Whether the Details tab replaces the chat this frame.
    fn shows_details(&self) -> bool {
        self.shows_tabs()
            && self
                .inspector
                .as_ref()
                .is_some_and(|inspected| inspected.view.tab == SessionTab::Details)
    }

    /// Show the which-key strip while normal mode is on.
    pub fn normal_mode(mut self, normal_mode: Option<NormalView>) -> Self {
        self.normal_mode = normal_mode;
        self
    }

    pub fn pending_images(mut self, images: &'a mut Vec<ImageAttachment>) -> Self {
        self.pending_images = Some(images);
        self
    }

    pub fn last_activity(mut self, ts: Option<u64>) -> Self {
        self.last_activity = ts;
        self
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

    pub fn is_working(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsWorking, val);
        self
    }

    pub fn dispatch_state(mut self, state: crate::session::DispatchState) -> Self {
        self.dispatch_state = state;
        self
    }

    /// Whether the current turn has produced content yet. Feeds `queued_from` so
    /// the queued indicator stays correct through a tool-using turn.
    pub fn turn_has_content(mut self, val: bool) -> Self {
        self.turn_has_content = val;
        self
    }

    pub fn has_pending_permission(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::HasPendingPerm, val);
        self
    }

    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
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

    /// Mark this as a Claude subagent's virtual session (see
    /// [`crate::session::SubagentOf`]).
    pub fn is_subagent(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsSubagent, val);
        self
    }

    /// Why this subagent's session can't be messaged now, if it can't
    /// ([`crate::session::composer_block`]): its composer is disabled, with
    /// the reason on hover.
    pub fn composer_block(mut self, reason: Option<&'static str>) -> Self {
        self.composer_block = reason;
        self
    }

    /// Mark this as a pending placeholder (a remote spawn/resume in flight), which
    /// renders the "connecting…" status line + spinner instead of a bare chat.
    pub fn is_connecting(mut self, val: bool) -> Self {
        self.flags.set(DaveUiFlags::IsConnecting, val);
        self
    }

    pub fn status_dot_color(mut self, color: Option<egui::Color32>) -> Self {
        self.status_dot_color = color;
        self
    }

    pub fn focus_queue_info(mut self, info: Option<FocusQueueInfo>) -> Self {
        self.focus_queue_info = info;
        self
    }

    pub fn run_configs(mut self, configs: &'a [crate::config::RunConfig]) -> Self {
        self.run_configs = configs;
        self
    }

    pub fn running_config_ids(
        mut self,
        ids: Option<&'a std::collections::HashSet<String>>,
    ) -> Self {
        self.running_config_ids = ids;
        self
    }

    /// The context window of `model` (the default window for `None`), which
    /// the status bar's meter and the Usage card fill against.
    pub fn model(mut self, model: Option<&str>) -> Self {
        self.context_window = crate::messages::context_window_for_model(model);
        self
    }

    /// The account's rate-limit windows, for the Rate limits card and the
    /// status bar's pill.
    pub fn rate_limit(mut self, rate_limit: Option<crate::messages::RateLimitInfo>) -> Self {
        self.rate_limit = rate_limit;
        self
    }

    /// Pin the clock the Details tab reads (see `now_unix` on the struct).
    pub fn now_unix(mut self, now_unix: u64) -> Self {
        self.now_unix = Some(now_unix);
        self
    }

    fn chat_margin(&self, ctx: &egui::Context) -> i8 {
        if notedeck::ui::is_narrow(ctx) {
            8
        } else {
            20
        }
    }

    fn chat_frame(&self, ctx: &egui::Context) -> egui::Frame {
        let margin = self.chat_margin(ctx);
        // The tab strip already clears the header; without it the chat leaves
        // room for the header itself.
        let top = if self.shows_tabs() {
            notedeck::tokens::SPACING_MD as i8
        } else {
            50
        };
        egui::Frame::new().inner_margin(egui::Margin {
            left: margin,
            right: margin,
            top,
            bottom: 0,
        })
    }

    /// The main render function. Call this to render Dave
    pub fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Clear the Truncate the enclosing `StripBuilder` cell sets when
        // clip=true (`egui_extras`: "better to truncate than hard clip"). `None`
        // rather than `Some(Wrap)`: what we want back is egui's default — each
        // `Ui` derives wrapping from its own layout — not a blanket rule that
        // outranks it. A style override reaches *inside* every descendant
        // widget, so forcing `Wrap` here made labels wrap in non-wrapping
        // horizontal rows, folding inline chips into tall blocks.
        ui.style_mut().wrap_mode = None;

        let result = top_buttons_ui(app_ctx, ui, self.status_dot_color);

        // Render session details inline, to the right of the buttons
        if let Some(details) = self.details {
            let max_width = ui.max_rect().right() - result.right_edge_x;
            if max_width > 50.0 {
                let details_rect = egui::Rect::from_min_size(
                    egui::pos2(result.right_edge_x, result.y),
                    egui::vec2(max_width, 32.0),
                );
                // The transaction the title's references resolve in, dropped
                // with the header: nostrdb allows one per thread, and the chat
                // below opens its own.
                let mut note_ctx = app_ctx.note_context();
                let txn = Transaction::new(note_ctx.ndb).ok();
                let mut refs = txn.as_ref().map(|txn| TitleRefs {
                    note: &mut note_ctx,
                    txn,
                });
                let truncation = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(details_rect), |ui| {
                        ui.set_clip_rect(details_rect);
                        session_header_ui(
                            ui,
                            details,
                            self.backend_type,
                            self.flags.contains(DaveUiFlags::IsSubagent),
                            refs.as_mut(),
                        )
                    })
                    .inner;

                if let Some(cwd) = &truncation.full {
                    let hover_resp = ui.interact(
                        details_rect,
                        egui::Id::unique("session_header_hover"),
                        egui::Sense::hover(),
                    );
                    hover_resp.on_hover_text_at_pointer(cwd);
                }
            }
        }

        let action = result.action;

        // The `Chat | Details` tab strip, under the header.
        let action = if self.shows_tabs() {
            let margin = self.chat_margin(ui.ctx());
            let tabs_action = self.inspector.as_ref().and_then(|inspected| {
                ui.horizontal(|ui| {
                    ui.add_space(margin as f32);
                    session_tabs_ui(inspected.view, app_ctx.i18n, ui)
                })
                .inner
            });
            ui.separator();
            tabs_action.or(action)
        } else {
            action
        };
        let shows_details = self.shows_details();

        egui::Frame::NONE
            .show(ui, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let margin = self.chat_margin(ui.ctx());
                    let bottom_margin = 100;

                    // Details has no input box, only the input's bottom
                    // margin under the status bar.
                    let mut r = if shows_details {
                        ui.add_space(bottom_margin as f32);
                        DaveResponse::default()
                    } else {
                        egui::Frame::new()
                            .outer_margin(egui::Margin {
                                left: margin,
                                right: margin,
                                top: 0,
                                bottom: bottom_margin,
                            })
                            .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_SM as i8))
                            .fill(input_fill(ui.visuals(), self.normal_mode.is_some()))
                            .corner_radius(notedeck::tokens::RADIUS_LG)
                            .show(ui, |ui| self.inputbox(app_ctx, ui))
                            .inner
                    };

                    {
                        let has_git = self.git_status.is_some();
                        let is_agentic = self.ai_mode == AiMode::Agentic;

                        // Show status bar when there's git status or badges to display
                        if has_git || is_agentic {
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
                                        .show(ui, |ui| self.status_bar_ui(app_ctx.i18n, ui))
                                        .inner
                                })
                                .inner;

                            if let Some(action) = badge_action {
                                r = DaveResponse::new(action).or(r);
                            }
                        }
                    }

                    if shows_details {
                        let branch = self
                            .git_status
                            .as_deref()
                            .and_then(|git| git.current())
                            .and_then(|status| status.work_tree())
                            .and_then(|tree| tree.branch.as_deref());
                        let details = self.details;
                        let permission_mode = self.permission_mode;
                        let backend_type = self.backend_type;
                        let session_id = self.session_id;
                        let is_remote = self.flags.contains(DaveUiFlags::IsRemote);
                        let context_window = self.context_window;
                        let rate_limit = self.rate_limit;
                        let now_unix = self.now_unix.unwrap_or_else(crate::session::now_unix);
                        let details_action = self.inspector.as_mut().and_then(|inspected| {
                            let facts = SessionFacts {
                                status: inspected.status,
                                waiting_on: inspected.waiting_on,
                                permission_mode,
                                backend_type,
                                details,
                                branch,
                                cli_session_id: inspected.cli_session_id,
                                is_remote,
                                context_window,
                                rate_limit,
                                now_unix,
                                chat: self.chat,
                                permissions: inspected.permissions,
                            };
                            // The Outputs card draws its cards as the chat's
                            // inline chips, which resolve through ndb.
                            let mut note_ctx = app_ctx.note_context();
                            let txn = Transaction::new(note_ctx.ndb).ok();
                            // Scoped by session, so each keeps its own grid
                            // scroll.
                            let id = ("dave_inspector", session_id);
                            ui.push_id(id, |ui| {
                                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                    inspector_ui(
                                        inspected.inspector,
                                        &facts,
                                        inspected.view,
                                        &mut note_ctx,
                                        txn.as_ref(),
                                        ui,
                                    )
                                })
                                .inner
                            })
                            .inner
                        });
                        return DaveResponse {
                            action: details_action,
                        }
                        .or(r);
                    }

                    let chat_response = egui::ScrollArea::vertical()
                        .id_salt(("dave_chat_scroll", self.session_id))
                        // Browsing the transcript with the block cursor must not
                        // be yanked back to the tail every frame; without a
                        // cursor, a live chat still follows its own output.
                        .stick_to_bottom(self.nav.cursor().is_none())
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
                egui::RichText::new(format!("An error occurred: {err}")).weak(),
            ));
        }
    }

    /// Render a chat message (user, assistant, tool call/response, etc)
    fn render_chat(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let mut response = DaveResponse::default();
        let is_agentic = self.ai_mode == AiMode::Agentic;

        // Collapsible blocks re-register themselves below, in visual order.
        self.nav.begin_frame();

        // Where queued (not-yet-dispatched) user messages start. Lives in
        // session.rs so it is testable without a render pass.
        let queued_from = crate::session::queued_from(
            self.chat,
            self.flags.contains(DaveUiFlags::IsWorking),
            self.dispatch_state,
            self.turn_has_content,
        );

        for (i, message) in self.chat.iter().enumerate() {
            self.nav.enter_row(i);
            match message {
                Message::Error(err) => {
                    self.error_chat(ctx.i18n, err, ui);
                }
                Message::User(msg) => {
                    let is_queued = queued_from.is_some_and(|qi| i >= qi);
                    user_message_ui(msg, is_queued, ctx, ui);
                }
                Message::Assistant(msg) => {
                    assistant_message_ui(msg, ctx, ui);
                }
                Message::ToolRunning(running) => {
                    // In-flight tool rows only appear in agentic mode, matching
                    // the completed ToolResponse row.
                    if is_agentic {
                        Self::tool_running_ui(running, ui);
                    }
                }
                Message::ToolResponse(msg) => {
                    Self::tool_response_ui(msg, is_agentic, self.nav, ui);
                }
                Message::System(text) => {
                    Self::system_message_ui(text, is_agentic, ui);
                }
                Message::ToolCalls(toolcalls) => {
                    if let Some(note_action) = Self::tool_calls_ui(ctx, toolcalls, ui) {
                        response = DaveResponse::note(note_action);
                    }
                }
                Message::PermissionRequest(request) => {
                    // Permission requests only in Agentic mode
                    if is_agentic {
                        if let Some(action) =
                            self.permission_request_ui(request, Some(&mut *ctx), ui)
                        {
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
                    if is_agentic && Self::subagent_ui(info, self.nav, ui) {
                        response =
                            DaveResponse::new(DaveAction::OpenSubagent(info.task_id.clone()));
                    }
                }
                Message::TodoUpdate(_) => {
                    // TodoUpdate is displayed in the task list sidebar, not in chat
                }
            };
        }

        // Show status line at the bottom of chat when working or compacting
        let status_text = if is_agentic && self.flags.contains(DaveUiFlags::IsCompacting) {
            Some("compacting...")
        } else if self.flags.contains(DaveUiFlags::IsWorking) {
            Some("computing...")
        } else if self.flags.contains(DaveUiFlags::IsConnecting) {
            // A pending placeholder (remote spawn/resume in flight): show progress
            // until the real kind-31988 state arrives and upgrades it.
            Some("connecting...")
        } else {
            None
        };

        if let Some(status) = status_text {
            ui.horizontal(|ui| {
                ui.add(SquareLoadingSpinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(status)
                        .color(ui.visuals().weak_text_color())
                        .italics(),
                );
                // Remote sessions can now be interrupted too — the Esc keystroke
                // publishes a command the host applies to its backend.
                ui.label(
                    egui::RichText::new("(press esc to interrupt)")
                        .color(ui.visuals().weak_text_color())
                        .small(),
                );
            });
        }

        response
    }

    fn tool_response_ui(
        tool_response: &ToolResponse,
        is_agentic: bool,
        nav: &mut BlockNav,
        ui: &mut egui::Ui,
    ) {
        match tool_response.responses() {
            ToolResponses::ExecutedTool(result) if is_agentic => {
                Self::executed_tool_ui(result, nav, ui);
            }
            _ => {
                //ui.label(format!("tool_response: {:?}", tool_response));
            }
        }
    }

    /// Render a compact approval card for structured `requestUserInput` prompts.
    ///
    /// Returns `true` when the payload matched the expected shape and was rendered.
    fn approval_prompt_ui(
        &self,
        request: &PermissionRequest,
        prompt: &ApprovalPromptInput,
        inner_margin: f32,
        corner_radius: f32,
        ui: &mut egui::Ui,
        action: &mut Option<DaveAction>,
    ) -> bool {
        if prompt.questions.is_empty() {
            return false;
        }

        egui::Frame::new()
            .fill(ui.visuals().widgets.noninteractive.bg_fill)
            .inner_margin(inner_margin)
            .corner_radius(corner_radius)
            .stroke(egui::Stroke::new(1.0_f32, ui.visuals().warn_fg_color))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(&request.tool_name).strong());
                ui.add_space(6.0);

                for (idx, question) in prompt.questions.iter().enumerate() {
                    if idx > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                    }

                    if let Some(header) = question.header.as_deref().filter(|text| !text.is_empty())
                    {
                        ui.label(egui::RichText::new(header).small().weak());
                    }

                    let question_text = question
                        .question
                        .as_deref()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("Allow this action?");
                    ui.add(
                        egui::Label::new(egui::RichText::new(question_text))
                            .wrap_mode(egui::TextWrapMode::Wrap),
                    );
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    self.permission_buttons(request, ui, action);
                });
            });

        true
    }

    /// Render a permission request with Allow/Deny buttons or response state
    fn permission_request_ui(
        &mut self,
        request: &PermissionRequest,
        ctx: Option<&mut AppContext>,
        ui: &mut egui::Ui,
    ) -> Option<DaveAction> {
        let mut action = None;

        let inner_margin = notedeck::tokens::SPACING_SM;
        let corner_radius = notedeck::tokens::RADIUS_SM;
        let spacing_x = 8.0;

        ui.spacing_mut().item_spacing.x = spacing_x;

        match request.response {
            Some(PermissionResponseType::Allowed) => {
                // Check if this is an answered shared question set with stored summary
                if let Some(summary) = &request.answer_summary {
                    super::ask_user_question_summary_ui(summary, ui);
                    return None;
                }

                // Responded state: Allowed (generic fallback)
                responded_permission_ui(
                    request,
                    "Allowed",
                    egui::Color32::from_rgb(100, 180, 100),
                    inner_margin,
                    corner_radius,
                    self.nav,
                    ui,
                );
            }
            Some(PermissionResponseType::Denied) => {
                // Responded state: Denied
                responded_permission_ui(
                    request,
                    "Denied",
                    egui::Color32::from_rgb(200, 100, 100),
                    inner_margin,
                    corner_radius,
                    self.nav,
                    ui,
                );
            }
            None => {
                if request.view.is_plan_review() {
                    return self.exit_plan_mode_ui(request, ctx, ui);
                }

                if let PermissionView::QuestionSet(questions) = &request.view {
                    if let (Some(answers_map), Some(index_map)) =
                        (&mut self.question_answers, &mut self.question_index)
                    {
                        return super::ask_user_question_ui(
                            request,
                            questions,
                            answers_map,
                            index_map,
                            ui,
                        );
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
                        .stroke(egui::Stroke::new(
                            notedeck::tokens::STROKE_THIN,
                            ui.visuals().warn_fg_color,
                        ))
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
                } else if let Some(prompt) = request.view.approval_prompt() {
                    if !self.approval_prompt_ui(
                        request,
                        prompt,
                        inner_margin,
                        corner_radius,
                        ui,
                        &mut action,
                    ) {
                        return None;
                    }
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
                        .stroke(egui::Stroke::new(
                            notedeck::tokens::STROKE_THIN,
                            ui.visuals().warn_fg_color,
                        ))
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
                                // Fallback: show JSON when we don't have a better renderer.
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

                // Always button (blue) — allow and don't ask again this session
                let always_response = super::badge::ActionButton::new(
                    "Always",
                    egui::Color32::from_rgb(30, 100, 180),
                    button_text_color,
                )
                .keybind("3")
                .show(ui)
                .on_hover_text("Press 3 to allow always for this session, Shift+3 with message");

                if deny_response.clicked() {
                    if shift_held {
                        *action = Some(DaveAction::TentativeDeny);
                    } else {
                        *action = Some(DaveAction::PermissionResponse {
                            request_id: request.id,
                            response: PermissionResponse::Deny {
                                reason: DEFAULT_DENY_REASON.into(),
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

                if always_response.clicked() {
                    if shift_held {
                        *action = Some(DaveAction::TentativeAllowAlways);
                    } else {
                        *action = Some(DaveAction::AllowAlways {
                            request_id: request.id,
                        });
                    }
                }

                // Exit button (orange) — deny permission and stop the turn
                let exit_response = super::badge::ActionButton::new(
                    "Exit",
                    egui::Color32::from_rgb(200, 140, 30),
                    button_text_color,
                )
                .show(ui)
                .on_hover_text("Deny this tool call and return to chat");

                if exit_response.clicked() {
                    *action = Some(DaveAction::ExitToolCall {
                        request_id: request.id,
                    });
                }

                add_msg_link(ui, shift_held, action);
            }
        });
    }

    /// Render ExitPlanMode tool call with Approve/Reject buttons
    fn exit_plan_mode_ui(
        &self,
        request: &PermissionRequest,
        ctx: Option<&mut AppContext>,
        ui: &mut egui::Ui,
    ) -> Option<DaveAction> {
        let mut action = None;
        let inner_margin = notedeck::tokens::SPACING_MD;
        let corner_radius = notedeck::tokens::RADIUS_MD;

        egui::Frame::new()
            .fill(ui.visuals().widgets.noninteractive.bg_fill)
            .inner_margin(inner_margin)
            .corner_radius(corner_radius)
            .stroke(egui::Stroke::new(
                notedeck::tokens::STROKE_THIN,
                ui.visuals().selection.stroke.color,
            ))
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

                    // Render plan content as markdown (pre-parsed at construction).
                    // `ctx` is `Some` from the live render path and `None` from
                    // the ctx-less test harnesses; with it, refs in a plan body
                    // resolve to their inline chips like the other surfaces.
                    if let Some(plan) = request.view.plan_markdown() {
                        // Plan rendering holds no transaction; open one (when we
                        // have a context) so refs in the plan body resolve.
                        let mut note_ctx = ctx.map(|c| c.note_context());
                        let txn = note_ctx
                            .as_ref()
                            .and_then(|nc| Transaction::new(nc.ndb).ok());
                        markdown_ui::render_parsed_markdown(
                            &plan.elements,
                            None,
                            &plan.source,
                            note_ctx.as_mut(),
                            txn.as_ref(),
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
    /// Render the compact `▶ ToolName summary` row shared by every tool-result
    /// layout. `arrow` is `Some` for a collapsible result (there's a body to
    /// reveal) and `None` for a plain one-liner. Returns the row response so a
    /// collapsible caller can sense clicks on it.
    /// A compact one-line header shared by every tool result. The summary is
    /// *truncated* (ellipsized) to the visible width rather than wrapped, so a
    /// long Bash command — for Bash the summary is the full command, kept
    /// untruncated by `format_bash_summary` — can never overrun the row or push
    /// the layout past the viewport. The full command is revealed in the
    /// expanded body (`tool_command_output_ui`).
    ///
    /// Pass `disclosure = Some(..)` for a collapsible row: the row leads with a
    /// ▶/▼ chevron and the *whole line* (full available width, not just the
    /// chevron) becomes a click target with a pointer cursor and a subtle hover
    /// highlight — the returned response is that line-level click. Pass `None`
    /// for a plain, non-interactive one-liner.
    ///
    /// `line_count` is `Some` for a result carrying free-form output: the row
    /// ends `· 523 lines` so its size shows even while collapsed. The summary
    /// truncates to leave room for it.
    fn exec_tool_header_ui(
        tool_name: &str,
        summary: &str,
        line_count: Option<usize>,
        disclosure: Option<Disclosure>,
        ui: &mut egui::Ui,
    ) -> egui::Response {
        // Reserve a paint slot *behind* the row content so a hover highlight can
        // be filled in underneath the text once we know the interaction state.
        let bg_idx = ui.painter().add(egui::Shape::Noop);
        let full_width = ui.available_width();

        let content = ui.horizontal(|ui| {
            if let Some(disclosure) = disclosure {
                let arrow = if disclosure.expanded { "▼" } else { "▶" };
                ui.add(egui::Label::new(
                    egui::RichText::new(arrow)
                        .size(10.0)
                        .color(ui.visuals().text_color().gamma_multiply(0.5)),
                ));
            }
            ui.add(egui::Label::new(
                egui::RichText::new(tool_name)
                    .size(11.0)
                    .color(ui.visuals().text_color().gamma_multiply(0.6))
                    .monospace(),
            ));
            // Lay the size out first so the summary knows how much room to
            // leave it.
            let size = line_count.filter(|&n| n > 0).map(|n| {
                let text = if n == 1 {
                    "· 1 line".to_owned()
                } else {
                    format!("· {n} lines")
                };
                ui.fonts_mut(|f| {
                    f.layout_no_wrap(
                        text,
                        egui::FontId::monospace(11.0),
                        ui.visuals().text_color().gamma_multiply(0.4),
                    )
                })
            });
            if !summary.is_empty() {
                let reserved = size
                    .as_ref()
                    .map_or(0.0, |g| g.size().x + ui.spacing().item_spacing.x);
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - reserved).max(0.0));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(summary)
                                .size(11.0)
                                .color(ui.visuals().text_color().gamma_multiply(0.4))
                                .monospace(),
                        )
                        .truncate(),
                    );
                });
            }
            if let Some(size) = size {
                ui.add(egui::Label::new(size));
            }
        });

        let Some(disclosure) = disclosure else {
            return content.response;
        };

        // Sense a click over the *full-width* row rather than the wonky
        // content-width rect the horizontal response would give — the whole
        // line toggles the disclosure. The row is labels only (no interactive
        // children), so a line-level click can't swallow anything. Re-sensing
        // the horizontal response directly is unreliable, so allocate a
        // dedicated interaction region over the row (as `responded_permission`
        // does).
        let row_rect = egui::Rect::from_min_size(
            content.response.rect.min,
            egui::vec2(full_width, content.response.rect.height()),
        );
        let resp = ui.interact(row_rect, disclosure.click_id, egui::Sense::click());

        // The block cursor outranks hover: a keyboard-focused row stays lit
        // while the pointer wanders across it.
        let highlight = if disclosure.is_cursor {
            Some(block_cursor_fill(ui))
        } else if resp.hovered() {
            Some(ui.visuals().widgets.hovered.weak_bg_fill)
        } else {
            None
        };

        if let Some(fill) = highlight {
            paint_row_highlight(bg_idx, row_rect, fill, ui);
        }

        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// Lay out `text` as a subdued, break-anywhere monospace block bounded to
    /// `max_width`. Break-anywhere so a long line, or an unbreakable token like a
    /// path or hash, can never run off the right edge — plain word-wrap leaves
    /// those overflowing. `max_rows` stops it after that many wrapped rows,
    /// ending in `…`. Returns whether that cut anything.
    fn wrapped_mono_ui(
        text: &str,
        color: egui::Color32,
        max_width: f32,
        max_rows: Option<usize>,
        ui: &mut egui::Ui,
    ) -> bool {
        let mut job = egui::text::LayoutJob::single_section(
            text.to_owned(),
            egui::TextFormat {
                font_id: egui::FontId::monospace(TOOL_BODY_FONT_SIZE),
                color,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width,
            max_rows: max_rows.unwrap_or(usize::MAX),
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        let elided = galley.elided;
        ui.add(egui::Label::new(galley));
        elided
    }

    /// Render the expanded detail for a free-form tool result: an optional
    /// leading `command`/context line, a divider, then the captured `output`.
    /// The header row only shows a truncated one-liner, so this unified body is
    /// where the reader sees the detail.
    ///
    /// `command` is `Some` for a tool whose summary *is* the thing that ran and
    /// deserves echoing (Bash's command), and `None` for a tool whose summary
    /// already fully names the call in the header (WebSearch's query) — there,
    /// only the result body needs showing, without duplicating the summary.
    ///
    /// The body is clamped so one result can't take over the chat: the command
    /// to [`COMMAND_PREVIEW_ROWS`] rows, the output to its head and tail with a
    /// "N lines hidden" gap between (both measured once, in
    /// [`ToolOutput`](crate::tool_output::ToolOutput)). A "show all" footer
    /// opens the whole of both, the output in a bounded scroll area that lays
    /// out only the lines in sight. Everything here slices the stored text;
    /// nothing copies the whole output in a frame.
    ///
    /// Both command and output wrap *break-anywhere* within the visible width —
    /// not a column an adjacent wide element (a diff, a long path) may have
    /// stretched past the viewport — so nothing overflows horizontally.
    fn tool_command_output_ui(
        command: Option<&str>,
        output: &crate::tool_output::ToolOutput,
        show_all_id: egui::Id,
        ui: &mut egui::Ui,
    ) {
        let show_all = ui.data(|d| d.get_temp::<bool>(show_all_id).unwrap_or(false));
        let toggled = ui
            .indent("exec_output_body", |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_SM as i8))
                    .corner_radius(notedeck::tokens::RADIUS_LG)
                    .show(ui, |ui| {
                        let max_width = ui.available_width().min(ui.clip_rect().width());
                        let mut cut = output.clamp().is_some();
                        // The command reads brighter than its output so the two
                        // sections stay distinct within the one block.
                        if let Some(command) = command {
                            let color = ui.visuals().text_color().gamma_multiply(0.85);
                            if show_all {
                                Self::wrapped_mono_ui(command, color, max_width, None, ui);
                            } else {
                                let end = notedeck::abbrev::floor_char_boundary(
                                    command,
                                    COMMAND_PREVIEW_BYTES,
                                );
                                let elided = Self::wrapped_mono_ui(
                                    &command[..end],
                                    color,
                                    max_width,
                                    Some(COMMAND_PREVIEW_ROWS),
                                    ui,
                                );
                                cut |= elided || end < command.len();
                            }
                            if !output.body().is_empty() {
                                ui.add_space(notedeck::tokens::SPACING_SM);
                                ui.separator();
                                ui.add_space(notedeck::tokens::SPACING_SM);
                            }
                        }
                        if show_all {
                            Self::full_output_ui(output, show_all_id, max_width, ui);
                        } else {
                            Self::output_preview_ui(output, max_width, ui);
                        }
                        (show_all || cut) && Self::show_all_footer_ui(show_all, ui)
                    })
                    .inner
            })
            .inner;

        if toggled {
            ui.data_mut(|d| d.insert_temp(show_all_id, !show_all));
        }
    }

    /// The clamped output: the whole body when it fits, otherwise its head, a
    /// dim "┄┄ N lines hidden ┄┄" gap, and its tail.
    fn output_preview_ui(
        output: &crate::tool_output::ToolOutput,
        max_width: f32,
        ui: &mut egui::Ui,
    ) {
        let color = ui.visuals().text_color().gamma_multiply(0.75);
        let Some(clamp) = output.clamp() else {
            if !output.body().is_empty() {
                Self::wrapped_mono_ui(output.body(), color, max_width, None, ui);
            }
            return;
        };
        let text = output.as_str();
        Self::wrapped_mono_ui(
            &text[clamp.head],
            color,
            max_width,
            Some(OUTPUT_PREVIEW_ROWS),
            ui,
        );
        ui.add(egui::Label::new(
            egui::RichText::new(format!("┄┄ {} lines hidden ┄┄", clamp.hidden_lines))
                .size(TOOL_BODY_FONT_SIZE)
                .monospace()
                .color(ui.visuals().text_color().gamma_multiply(0.4)),
        ));
        Self::wrapped_mono_ui(
            &text[clamp.tail],
            color,
            max_width,
            Some(OUTPUT_PREVIEW_ROWS),
            ui,
        );
    }

    /// The whole output in a scroll area no taller than
    /// [`SHOW_ALL_VIEWPORT_FRACTION`] of the viewport. Lines don't wrap, so
    /// every row is the same height and only the rows in sight are laid out;
    /// a long line scrolls sideways instead.
    fn full_output_ui(
        output: &crate::tool_output::ToolOutput,
        id: egui::Id,
        max_width: f32,
        ui: &mut egui::Ui,
    ) {
        let font = egui::FontId::monospace(TOOL_BODY_FONT_SIZE);
        let row_height = ui.fonts_mut(|f| f.row_height(&font));
        let color = ui.visuals().text_color().gamma_multiply(0.75);
        let max_height = ui.ctx().content_rect().height() * SHOW_ALL_VIEWPORT_FRACTION;
        ui.set_max_width(max_width);
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::both()
            .id_salt(id.with("scroll"))
            .max_height(max_height)
            .auto_shrink([false, true])
            .show_rows(ui, row_height, output.line_count(), |ui, rows| {
                for i in rows {
                    let line = output.line(i);
                    let line =
                        &line[..notedeck::abbrev::floor_char_boundary(line, SHOW_ALL_LINE_BYTES)];
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).font(font.clone()).color(color))
                            .extend(),
                    );
                }
            });
    }

    /// The `▸ show all` / `▾ show less` footer under a clamped tool body.
    /// Returns whether it was clicked.
    fn show_all_footer_ui(show_all: bool, ui: &mut egui::Ui) -> bool {
        let text = if show_all {
            "▾ show less"
        } else {
            "▸ show all"
        };
        ui.add_space(notedeck::tokens::SPACING_SM);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .size(TOOL_BODY_FONT_SIZE)
                    .color(ui.visuals().text_color().gamma_multiply(0.5)),
            )
            .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    }

    /// Render an in-flight tool row: a spinner in place of the disclosure arrow,
    /// then the tool name and its call-time summary — the same subdued monospace
    /// styling as the completed `exec_tool_header_ui` row, so on completion the
    /// row is replaced in place with no visual jump.
    fn tool_running_ui(running: &RunningTool, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(SquareLoadingSpinner::new().size(11.0));
            ui.add(egui::Label::new(
                egui::RichText::new(&running.tool_name)
                    .size(11.0)
                    .color(ui.visuals().text_color().gamma_multiply(0.6))
                    .monospace(),
            ));
            if !running.summary.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&running.summary)
                            .size(11.0)
                            .color(ui.visuals().text_color().gamma_multiply(0.4))
                            .monospace(),
                    )
                    .truncate(),
                );
            }
        });
    }

    fn executed_tool_ui(result: &ExecutedTool, nav: &mut BlockNav, ui: &mut egui::Ui) {
        if let Some(file_update) = &result.file_update {
            // File edit with diff — expanded by default regardless of size. In
            // auto / accept-edits mode the CLI approves the edit itself, so no
            // permission row ever showed this diff: this row is the only place
            // the user sees what changed. A user toggle still collapses it.
            let expand_id = ui.scope_id().with("exec_diff").with(&result.summary);
            let block = nav.register(expand_id, true, ui);

            let header_resp = Self::exec_tool_header_ui(
                &result.tool_name,
                &result.summary,
                None,
                Some(Disclosure {
                    click_id: expand_id.with("header_click"),
                    expanded: block.expanded,
                    is_cursor: block.is_cursor,
                }),
                ui,
            );

            if block.scroll_into_view {
                header_resp.scroll_to_me(Some(egui::Align::Center));
            }

            if header_resp.clicked() {
                ui.data_mut(|d| d.insert_temp(expand_id, !block.expanded));
            }

            if block.expanded {
                diff::file_path_header(file_update, ui);
                diff::file_update_ui(file_update, false, ui);
            }
        } else if let Some(output) = &result.output {
            // Free-form tool output (bash stdout/stderr) — expanded by default so
            // the command and its output are visible without a click, mirroring
            // auto-accepted permission rows: the user never approved it up front,
            // so what ran should stay on-screen. A user toggle still collapses it.
            let expand_id = ui.scope_id().with("exec_output").with(&result.summary);
            let block = nav.register(expand_id, true, ui);

            let header_resp = Self::exec_tool_header_ui(
                &result.tool_name,
                &result.summary,
                Some(output.line_count()),
                Some(Disclosure {
                    click_id: expand_id.with("header_click"),
                    expanded: block.expanded,
                    is_cursor: block.is_cursor,
                }),
                ui,
            );

            if block.scroll_into_view {
                header_resp.scroll_to_me(Some(egui::Align::Center));
            }

            if header_resp.clicked() {
                ui.data_mut(|d| d.insert_temp(expand_id, !block.expanded));
            }

            if block.expanded {
                // Bash's summary *is* the command that ran, so echo it in full
                // above its output (stripping the one surrounding backtick pair
                // `format_bash_summary` adds). Other output-bearing tools —
                // WebSearch — already name the call fully in the header, so the
                // body shows only the result without duplicating the summary.
                let command = (result.tool_name == "Bash").then(|| {
                    result
                        .summary
                        .strip_prefix('`')
                        .and_then(|s| s.strip_suffix('`'))
                        .unwrap_or(&result.summary)
                });
                Self::tool_command_output_ui(command, output, expand_id.with("show_all"), ui);
            }
        } else {
            // Compact single-line display with subdued styling
            Self::exec_tool_header_ui(&result.tool_name, &result.summary, None, None, ui);
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

    /// A `system` row: in an agentic session, a notice the session wrote into
    /// its own chat, such as a slash command's reply (`/mcp`, `/context`) or
    /// "Model set to …". It is drawn as markdown in the weak text colour, so a
    /// `/context` reply shows its heading and table, with a right-click Copy.
    /// A chat-mode session's only `system` row is its system prompt, which
    /// stays hidden.
    ///
    /// Notices are the CLI's text, not the model's, so they carry no nostr
    /// references: the markdown is drawn without a note context.
    fn system_message_ui(system: &SystemMessage, is_agentic: bool, ui: &mut egui::Ui) {
        if !is_agentic {
            return;
        }
        let row = ui.scope(|ui| {
            ui.visuals_mut().override_text_color = Some(ui.visuals().weak_text_color());
            markdown_ui::render_parsed_markdown(
                system.parsed_elements(),
                None,
                system.text(),
                None,
                None,
                ui,
            );
        });
        message_menu(&row.response, None, system.text());
    }

    /// Render a single subagent's status with expandable tool results.
    /// Returns whether its "open" link was clicked, which switches to the
    /// subagent's own session.
    fn subagent_ui(info: &SubagentInfo, nav: &mut BlockNav, ui: &mut egui::Ui) -> bool {
        let tool_count = info.tool_results.len();
        let has_tools = tool_count > 0;
        let expand_id = subagent_block_id(&info.task_id);
        // Every subagent registers, so the Details tab can put the cursor on
        // it; one without tools has nothing to expand.
        let block = nav.register(expand_id, false, ui);
        let mut expanded = has_tools && block.expanded;

        // Reserve a paint slot *behind* the row so the block cursor can be
        // filled in underneath it once the row's rect is known. This row does
        // not go through `exec_tool_header_ui`, which does the same for hover.
        let bg_idx = ui.painter().add(egui::Shape::Noop);
        let full_width = ui.available_width();

        let row = ui.horizontal(|ui| {
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

            // A background subagent keeps running after the launching turn ends;
            // flag it as such until its wake-up completion lands.
            if info.background && info.status == SubagentStatus::Running {
                ui.label(
                    egui::RichText::new("· in background")
                        .size(10.0)
                        .italics()
                        .color(ui.visuals().text_color().gamma_multiply(0.5)),
                );
            }

            // Show spinner for running subagents
            if info.status == SubagentStatus::Running {
                ui.add(SquareLoadingSpinner::new().size(11.0));
            }

            // Tool count indicator (clickable to expand)
            if has_tools {
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
                    expanded = !expanded;
                    ui.data_mut(|d| d.insert_temp(expand_id, expanded));
                }
            }

            ui.add(
                egui::Label::new(
                    egui::RichText::new("open \u{203A}")
                        .size(10.0)
                        .color(ui.visuals().hyperlink_color),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Open this subagent's session")
            .clicked()
        });
        let open_clicked = row.inner;

        let row_rect = egui::Rect::from_min_size(
            row.response.rect.min,
            egui::vec2(full_width, row.response.rect.height()),
        );
        if block.is_cursor {
            paint_row_highlight(bg_idx, row_rect, block_cursor_fill(ui), ui);
        }
        if block.scroll_into_view {
            row.response.scroll_to_me(Some(egui::Align::Center));
        }

        // Expanded tool results. Their blocks register after this one, so a
        // subagent's nested rows sit in visual order behind it.
        if has_tools && expanded {
            ui.indent(("subagent_tools", &info.task_id), |ui| {
                for result in &info.tool_results {
                    Self::executed_tool_ui(result, nav, ui);
                }
            });
        }
        open_clicked
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
            jobs: ctx.media_jobs.sender(),
            unknown_ids: ctx.unknown_ids,
            nip05_cache: ctx.nip05_cache,
            style_cache: ctx.style_cache,
            time_cache: ctx.time_cache,
            clipboard: ctx.clipboard,
            i18n: ctx.i18n,
            global_wallet: ctx.global_wallet,
            sound: ctx.sound,
            registries: ctx.registries,
            app_actions: ctx.app_actions,
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
        if let Some(reason) = self.composer_block {
            return self.blocked_composer_ui(reason, ui);
        }
        let i18n = &mut *app_ctx.i18n;
        // Remote sessions can be interrupted too: the Stop button / normal `s`
        // publishes an interrupt command the host applies to its backend.
        let show_stop = self.flags.contains(DaveUiFlags::IsWorking);

        let mode = match self.normal_mode {
            Some(view) => InputMode::Normal {
                can_stop: view.interruptible,
            },
            None => InputMode::Insert,
        };

        let claim_popup_keys = self.slash.as_deref().is_some_and(|p| p.claims_keys());
        let mut layout = InputboxLayout::new(self.input, i18n, mode)
            .show_stop(show_stop)
            .claim_popup_keys(claim_popup_keys)
            .id(input_id(self.session_id));
        // A subagent's session messages the subagent, not dave.
        if self.flags.contains(DaveUiFlags::IsSubagent) && mode == InputMode::Insert {
            layout.hint_text = notedeck::tr!(
                i18n,
                "Message this subagent...",
                "Placeholder text for the input in a Claude subagent's session, which sends to that subagent"
            );
        }

        let result = layout.show(ui);

        if let Some(outcome) = self.slash_popup(&result, i18n, ui) {
            match outcome {
                crate::slash::SlashOutcome::Run => return DaveResponse::send(),
                crate::slash::SlashOutcome::Edited => {
                    let id = input_id(self.session_id);
                    crate::slash::cursor_to_end(ui.ctx(), id, self.input);
                    ui.memory_mut(|m| m.request_focus(id));
                }
                crate::slash::SlashOutcome::Moved => {}
            }
        }

        if let Some(r) = &result.text_response {
            dave_input_context(
                ui,
                r,
                app_ctx.clipboard,
                self.input,
                self.pending_images.as_deref_mut(),
            );

            // Request focus if flagged (e.g., after spawning a new agent or entering tentative state).
            // Skip on mobile to avoid popping up the virtual keyboard on every session switch.
            if *self.focus_requested {
                if !notedeck::ui::is_compiled_as_mobile() {
                    r.request_focus();
                }
                *self.focus_requested = false;
            }

            // Browser-style autofocus: grab focus when the chrome switches to
            // Dave for the first time and nothing was previously focused.
            notedeck_ui::autofocus(r, ui.ctx());

            // Unfocus text input when there's a pending permission request
            // UNLESS we're in tentative state (user needs to type message)
            let in_tentative_state = self.permission_message_state != PermissionMessageState::None;
            if self.flags.contains(DaveUiFlags::HasPendingPerm) && !in_tentative_state {
                r.surrender_focus();
            }

            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
            {
                return DaveResponse::send();
            }
        }

        // Thumbnail strip second — in bottom_up this renders ABOVE the text row.
        // allocate_ui_with_layout pre-positions the rect at the bottom_up cursor
        // (unlike with_layout which takes the full available rect) and forces
        // top_down so image_thumbnail_strip's internal widgets render correctly.
        if let Some(pending) = &mut self.pending_images {
            if !pending.is_empty() {
                // Height must closely match content: in bottom_up, next_space
                // reserves this much vertical room above the text row.
                let strip_height = THUMBNAIL_SIZE + ui.spacing().item_spacing.y;
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), strip_height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        image_thumbnail_strip(pending, ui);
                    },
                );
            }
        }

        result.action()
    }

    /// Draw the slash popup over the input while the input has focus (or
    /// had it last frame: a click on a row takes it away), reranked for
    /// whatever was typed this frame.
    fn slash_popup(
        &mut self,
        input: &InputboxResult,
        i18n: &mut notedeck::Localization,
        ui: &mut egui::Ui,
    ) -> Option<crate::slash::SlashOutcome> {
        let popup = self.slash.as_deref_mut()?;
        popup.sync(self.input);
        let id = input_id(self.session_id);
        let focused = ui.memory(|m| m.has_focus(id) || m.had_focus_last_frame(id));
        if !focused || self.normal_mode.is_some() {
            return None;
        }
        // The session's pick, or the CLI's own default when it has none.
        let current_model = self
            .details
            .and_then(|d| d.requested_model.as_deref())
            .unwrap_or("default");
        super::slash_popup::slash_popup_ui(popup, self.input, current_model, input.rect, i18n, ui)
    }

    /// A composer that can't send now, saying why on hover (see
    /// [`crate::session::composer_block`]). Stop still works while the
    /// subagent runs.
    fn blocked_composer_ui(&mut self, reason: &'static str, ui: &mut egui::Ui) -> DaveResponse {
        let button_size = egui::vec2(60.0, notedeck::tokens::BUTTON_LG);
        let show_stop = self.flags.contains(DaveUiFlags::IsWorking);
        // The height a one-line input box takes (see `InputboxLayout::show`).
        let height = notedeck::tokens::BUTTON_LG + 20.0;
        let stop_clicked = ui
            .allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let stop_clicked = show_stop
                        && ui
                            .add(egui::Button::new("Stop").min_size(button_size))
                            .clicked();
                    ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(self.input)
                            .desired_width(f32::INFINITY)
                            .hint_text(
                                egui::RichText::new("this subagent can't be messaged now").weak(),
                            ),
                    )
                    .on_disabled_hover_text(reason);
                    stop_clicked
                })
                .inner
            })
            .inner;
        if stop_clicked {
            DaveResponse::new(DaveAction::Interrupt)
        } else {
            DaveResponse::none()
        }
    }
}

/// A `user` message a person typed: a filled bubble on the right, with a
/// right-click menu that shows when it was sent and copies its text. A message
/// another session sent draws as [`from_session_message_ui`] instead.
#[profiling::function]
pub fn user_message_ui(
    msg: &crate::messages::UserMessage,
    is_queued: bool,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    if let Some(from) = msg.from_session.as_deref() {
        from_session_message_ui(msg, from, is_queued, ctx, ui);
        return;
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        let bubble = egui::Frame::new()
            .inner_margin(10.0)
            .corner_radius(10.0)
            .fill(ui.visuals().widgets.inactive.weak_bg_fill)
            .show(ui, |ui| user_message_body(msg, is_queued, ctx, ui))
            .response;
        message_menu(&bubble, msg.sent_at, &msg.text);
    });
}

/// An assistant reply: its streamed markdown, with a right-click menu that
/// shows when it was written and copies its text.
#[profiling::function]
pub fn assistant_message_ui(msg: &AssistantMessage, ctx: &mut AppContext, ui: &mut egui::Ui) {
    let elements = msg.parsed_elements();
    let partial = msg.partial();
    let buffer = msg.buffer();
    // Chat rendering holds no transaction; open one here and thread it in so
    // the streamed markdown can resolve inline references.
    let mut note_ctx = ctx.note_context();
    let txn = Transaction::new(note_ctx.ndb).expect("markdown txn");
    let r = ui.scope(|ui| {
        markdown_ui::render_parsed_markdown(
            elements,
            partial,
            buffer,
            Some(&mut note_ctx),
            Some(&txn),
            ui,
        );
    });
    message_menu(&r.response, Some(msg.sent_at()), msg.text());
}

/// The right-click menu on a chat message's `content`: when it was sent, if
/// known, then Copy, which copies `text`.
///
/// It is a container menu, so it opens over a chip or link inside the message
/// without taking their clicks (see
/// [`container_context_menu`](notedeck_ui::context_menu::container_context_menu)).
fn message_menu(content: &egui::Response, sent_at: Option<u64>, text: &str) {
    notedeck_ui::context_menu::container_context_menu(content, |ui| {
        if let Some(label) = sent_at.and_then(sent_at_label) {
            ui.label(egui::RichText::new(label).small().weak());
        }
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text.to_owned());
            ui.close();
        }
    });
}

/// When a message was sent, in local time, for its right-click menu:
/// "Oct 13, 2026 · 2:04 PM". `None` for a time chrono can't represent.
fn sent_at_label(sent_at: u64) -> Option<String> {
    let utc = chrono::DateTime::from_timestamp(i64::try_from(sent_at).ok()?, 0)?;
    Some(
        utc.with_timezone(&chrono::Local)
            .format("%b %-d, %Y · %-I:%M %p")
            .to_string(),
    )
}

/// A `user` message another session sent (`agentium send` from inside a
/// session): a full-width card with a thin violet outline, headed by a
/// small violet "from" and the sender's `agentium:` chip, so it can't be
/// mistaken for a message a person typed. The chip is live: it shows the
/// sender's current title and a click switches to that session. A sender
/// whose state isn't in the local db yet shows as its bare ref.
///
/// `from` is the sender's ref, the message's
/// [`from_session`](crate::messages::UserMessage::from_session).
#[profiling::function]
pub fn from_session_message_ui(
    msg: &crate::messages::UserMessage,
    from: &str,
    is_queued: bool,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    let content = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(13, 10))
        .corner_radius(8.0)
        .stroke(egui::Stroke::new(
            1.0,
            FROM_SESSION_VIOLET.gamma_multiply(0.4),
        ))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("from")
                        .small()
                        .color(FROM_SESSION_VIOLET),
                );
                let mut note_ctx = ctx.note_context();
                let txn = Transaction::new(note_ctx.ndb).expect("from-session txn");
                let drawn = notedeck_ui::markdown::render_reference(
                    ui,
                    &mut note_ctx,
                    &txn,
                    from,
                    notedeck::RenderContext::Inline,
                );
                if !drawn {
                    ui.label(egui::RichText::new(from).small().color(FROM_SESSION_VIOLET));
                }
            });
            user_message_body(msg, is_queued, ctx, ui);
        })
        .response;
    message_menu(&content, msg.sent_at, &msg.text);
}

/// The accent of a message another session sent: its card's outline and its
/// "from" header (see [`from_session_message_ui`]).
pub(crate) const FROM_SESSION_VIOLET: egui::Color32 = egui::Color32::from_rgb(0xA9, 0x8B, 0xF0);

/// A `user` message's content, shared by the bubble a person's message sits in
/// and the card another session's does: its images, its text as ref-aware
/// markdown, and a "queued" note while it waits for the turn to end.
fn user_message_body(
    msg: &crate::messages::UserMessage,
    is_queued: bool,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    for img in &msg.images {
        ui.add(
            egui::Image::from_bytes(img.egui_uri(), img.bytes.clone())
                .max_size(egui::vec2(200.0, 200.0))
                .corner_radius(4.0),
        );
    }
    if !msg.text.is_empty() {
        // Ref-aware markdown so a headway ref the user types (bare or
        // backtick-wrapped) renders as its inline chip, matching the assistant
        // surface. Chat rendering holds no transaction, so open one here and
        // pass it in.
        let mut note_ctx = ctx.note_context();
        let txn = Transaction::new(note_ctx.ndb).expect("markdown txn");
        markdown_ui::render_markdown_with_refs(ui, &mut note_ctx, &txn, &msg.text);
    }
    if is_queued {
        ui.label(
            egui::RichText::new("queued")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
    }
}

/// Paste text into the current input buffer using Dave's append behavior.
fn append_clipboard_text(clipboard: &mut egui_winit::clipboard::Clipboard, input: &mut String) {
    if let Some(text) = clipboard.get() {
        input.push_str(&text);
    }
}

/// Result of rendering an InputboxLayout.
pub struct InputboxResult {
    pub ask_clicked: bool,
    pub stop_clicked: bool,
    pub text_response: Option<egui::Response>,
    /// The whole input box, buttons included: what the slash popup sits on.
    pub rect: egui::Rect,
}

impl InputboxResult {
    /// The action these buttons raise, if any.
    ///
    /// The single place an inputbox click becomes a [`DaveAction`]. Lifted out
    /// of [`DaveUi::inputbox`], which needs an `AppContext` — this seam lets a
    /// test click the real Stop button and follow the click all the way to the
    /// action it raises, instead of asserting a mapping it restates itself.
    pub fn action(&self) -> DaveResponse {
        if self.ask_clicked {
            return DaveResponse::send();
        }
        if self.stop_clicked {
            return DaveResponse::new(DaveAction::Interrupt);
        }
        DaveResponse::none()
    }
}

/// Which vim mode the chat input is in, which decides what it says and how
/// loudly it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// The default: the input has focus and keys type into it.
    Insert,
    /// Normal mode: the input gave up focus and bare keys are commands.
    Normal {
        /// Whether `s` would stop a running turn right now, so the cue only
        /// offers it when it would do something.
        can_stop: bool,
    },
}

/// The chat input's background: its usual fill, or in normal mode halfway to
/// the panel behind it, so a mode that swallows typing reads as "not
/// listening" at the spot the user is looking. Only the colour changes, so
/// entering or leaving the mode never moves anything.
pub(crate) fn input_fill(visuals: &egui::Visuals, normal_mode: bool) -> egui::Color32 {
    if !normal_mode {
        return visuals.extreme_bg_color;
    }
    visuals
        .extreme_bg_color
        .lerp_to_gamma(visuals.panel_fill, 0.5)
}

/// Extracted inputbox layout used by both DaveUi and tests.
///
/// Renders the Ask/Stop buttons and text input in a right-to-left layout.
pub struct InputboxLayout<'a> {
    pub input: &'a mut String,
    pub ask_label: String,
    pub stop_label: String,
    /// The placeholder: "Ask dave anything..." in insert mode, the
    /// `-- NORMAL --` cue in normal mode.
    pub hint_text: String,
    pub show_stop: bool,
    pub id: Option<egui::Id>,
    /// Normal mode dims whatever is typed, so it reads as not taking keys.
    pub mode: InputMode,
    /// The slash popup is open over the input and takes Tab, Esc and the
    /// arrows (see [`claim_popup_keys`](Self::claim_popup_keys)).
    pub claim_popup_keys: bool,
}

impl<'a> InputboxLayout<'a> {
    /// The input as `mode` shows it. Normal mode swaps the placeholder for its
    /// cue rather than adding a line, so it costs the same one `tr!` a frame as
    /// insert mode and takes no extra room.
    pub fn new(input: &'a mut String, i18n: &mut notedeck::Localization, mode: InputMode) -> Self {
        let hint_text = match mode {
            InputMode::Insert => notedeck::tr!(
                i18n,
                "Ask dave anything...",
                "Placeholder text for Dave AI input field"
            ),
            InputMode::Normal { can_stop: true } => notedeck::tr!(
                i18n,
                "-- NORMAL -- i to type · s to stop · Esc for menu",
                "Dave chat input placeholder in vim normal mode while a turn is running: i returns to typing, s stops the turn, Esc opens the side menu"
            ),
            InputMode::Normal { can_stop: false } => notedeck::tr!(
                i18n,
                "-- NORMAL -- i to type · Esc for menu",
                "Dave chat input placeholder in vim normal mode: i returns to typing, Esc opens the side menu"
            ),
        };
        Self {
            input,
            ask_label: notedeck::tr!(i18n, "Ask", "Button to send message to Dave AI assistant"),
            stop_label: notedeck::tr!(i18n, "Stop", "Button to interrupt/stop the AI operation"),
            hint_text,
            show_stop: false,
            id: None,
            mode,
            claim_popup_keys: false,
        }
    }

    pub fn new_default(input: &'a mut String) -> Self {
        Self {
            input,
            ask_label: "Ask".to_string(),
            stop_label: "Stop".to_string(),
            hint_text: "Ask dave anything...".to_string(),
            show_stop: false,
            id: None,
            mode: InputMode::Insert,
            claim_popup_keys: false,
        }
    }

    pub fn show_stop(mut self, show: bool) -> Self {
        self.show_stop = show;
        self
    }

    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Keep focus through Tab and Esc while the slash popup takes them.
    /// egui reads both at the start of a frame, before any key handler runs:
    /// Tab would move focus to the next widget and Esc would drop it, unless
    /// the focused input's event filter says it wants them.
    pub fn claim_popup_keys(mut self, claim: bool) -> Self {
        self.claim_popup_keys = claim;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> InputboxResult {
        let line_count = self.input.lines().count().max(1).clamp(1, 8);
        let line_height = 20.0;
        let base_height = notedeck::tokens::BUTTON_LG;
        let input_height = base_height + (line_count as f32 * line_height);
        let button_size = egui::vec2(60.0, notedeck::tokens::BUTTON_LG);

        let inner = ui.allocate_ui(egui::vec2(ui.available_width(), input_height), |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ask_clicked = ui
                        .add(egui::Button::new(&self.ask_label).min_size(button_size))
                        .clicked();

                    let stop_clicked = self.show_stop
                        && ui
                            .add(egui::Button::new(&self.stop_label).min_size(button_size))
                            .clicked();

                    let text_response = egui::ScrollArea::vertical()
                        .max_height(ui.available_height())
                        .show(ui, |ui| {
                            let mut edit = egui::TextEdit::multiline(self.input)
                                .desired_width(f32::INFINITY)
                                .return_key(KeyboardShortcut::new(
                                    Modifiers {
                                        shift: true,
                                        ..Default::default()
                                    },
                                    Key::Enter,
                                ))
                                .hint_text(egui::RichText::new(&self.hint_text).weak())
                                .frame(
                                    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2)),
                                );

                            if self.mode != InputMode::Insert {
                                edit = edit.text_color(ui.visuals().weak_text_color());
                            }

                            if self.claim_popup_keys {
                                edit = edit.event_filter(egui::EventFilter {
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    tab: true,
                                    escape: true,
                                });
                            }

                            // The exact id, not a salt, so the slash popup's
                            // key step can ask whether this input has focus.
                            if let Some(id) = self.id {
                                edit = edit.id(id);
                            }

                            ui.add(edit)
                        })
                        .inner;

                    InputboxResult {
                        ask_clicked,
                        stop_clicked,
                        text_response: Some(text_response),
                        rect: egui::Rect::NOTHING,
                    }
                })
                .inner
            })
            .inner
        });
        InputboxResult {
            rect: inner.response.rect,
            ..inner.inner
        }
    }
}

/// Dave-specific input context menu that prefers image paste before text paste.
/// The chat input's widget id for `session_id`. Each session has its own
/// input, so focus follows the session that owns it.
pub(crate) fn input_id(session_id: SessionId) -> egui::Id {
    egui::Id::unique(("dave_input", session_id))
}

fn dave_input_context(
    ui: &mut egui::Ui,
    response: &egui::Response,
    clipboard: &mut egui_winit::clipboard::Clipboard,
    input: &mut String,
    pending_images: Option<&mut Vec<ImageAttachment>>,
) {
    response.context_menu(|ui| {
        if ui.button("Paste").clicked() {
            let pasted_image = pending_images.map(try_paste_image).unwrap_or(false);
            if !pasted_image {
                append_clipboard_text(clipboard, input);
            }
            ui.close();
        }

        if ui.button("Copy").clicked() {
            clipboard.set_text(input.to_owned());
            ui.close();
        }

        if ui.button("Cut").clicked() {
            clipboard.set_text(input.to_owned());
            input.clear();
            ui.close();
        }
    });

    if response.middle_clicked() {
        append_clipboard_text(clipboard, input);
    }

    notedeck_ui::include_input(ui, response);
}

/// Try to paste an image from the clipboard into `pending`.
/// Returns `true` if an image was staged; `false` if the clipboard had no image.
fn try_paste_image(pending: &mut Vec<ImageAttachment>) -> bool {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(err) => {
            tracing::debug!("paste_image: clipboard open failed: {}", err);
            return false;
        }
    };
    let img_data = match clipboard.get_image() {
        Ok(d) => d,
        Err(err) => {
            tracing::debug!("paste_image: no image in clipboard: {}", err);
            return false;
        }
    };
    let w = img_data.width as u32;
    let h = img_data.height as u32;
    tracing::debug!("paste_image: got {}x{} image from clipboard", w, h);
    let Some(rgba) = image::RgbaImage::from_raw(w, h, img_data.bytes.into_owned()) else {
        tracing::warn!("paste_image: RgbaImage::from_raw failed (bad dimensions?)");
        return false;
    };
    let mut png_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    if let Err(err) = rgba.write_to(&mut cursor, image::ImageFormat::Png) {
        tracing::warn!("paste_image: PNG encode failed: {}", err);
        return false;
    }
    tracing::debug!("paste_image: encoded {} PNG bytes", png_bytes.len());
    pending.push(ImageAttachment::new(png_bytes, "image/png"));
    true
}

const THUMBNAIL_SIZE: f32 = 100.0;

/// Render a horizontal strip of image thumbnails with ✕ remove buttons.
fn image_thumbnail_strip(pending: &mut Vec<ImageAttachment>, ui: &mut egui::Ui) {
    if pending.is_empty() {
        return;
    }

    let mut remove_idx = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, img) in pending.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(THUMBNAIL_SIZE, THUMBNAIL_SIZE),
                egui::Sense::hover(),
            );
            ui.put(
                rect,
                egui::Image::from_bytes(img.egui_uri(), img.bytes.clone())
                    .fit_to_exact_size(egui::vec2(THUMBNAIL_SIZE, THUMBNAIL_SIZE))
                    .corner_radius(6.0),
            );
            // Small × button in the top-right corner
            let btn_size = 18.0;
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - btn_size - 2.0, rect.top() + 2.0),
                egui::vec2(btn_size, btn_size),
            );
            if ui
                .put(
                    btn_rect,
                    egui::Button::new(egui::RichText::new("×").size(12.0).strong())
                        .corner_radius(btn_size / 2.0)
                        .min_size(egui::vec2(btn_size, btn_size)),
                )
                .clicked()
            {
                remove_idx = Some(i);
            }
        }
    });
    if let Some(idx) = remove_idx {
        pending.remove(idx);
    }
}

/// Extract a concise, human-readable summary of what a tool call operates on
/// (bash command, file path, or a single argument), borrowing from the tool
/// input so it stays allocation-free in the per-frame render path.
fn permission_operation_detail(tool_input: &serde_json::Value) -> Option<&str> {
    let obj = tool_input.as_object()?;

    // Bash and friends: the command being run.
    if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
        return Some(cmd);
    }

    // Edit/Write: the file being changed.
    if let Some(path) = obj.get("file_path").and_then(|v| v.as_str()) {
        return Some(path);
    }

    // Single-argument tools: the lone value.
    if obj.len() == 1 {
        if let Some(value) = obj.values().next().and_then(|v| v.as_str()) {
            return Some(value);
        }
    }

    None
}

/// The collapsible state of a tool-result header row: the id its line-level
/// click senses under, whether its body is showing, and whether the keyboard
/// block cursor is on it.
#[derive(Clone, Copy)]
struct Disclosure {
    click_id: egui::Id,
    expanded: bool,
    is_cursor: bool,
}

/// The wash the keyboard block cursor lays behind its row. The selection
/// colour at full strength swallows these rows — their text is deliberately
/// drawn at 40-60% alpha — so it goes on as a tint: unmistakable as the cursor,
/// still readable as a row. The session list washes its row the same way while
/// normal mode walks it.
pub(crate) fn block_cursor_fill(ui: &egui::Ui) -> egui::Color32 {
    notedeck::theme::selection_fill(ui.visuals()).gamma_multiply(0.35)
}

/// Fill a paint slot reserved *behind* a row, in the rounded, slightly
/// outdented shape every collapsible row highlights with — hover on a tool
/// header, and the block cursor on any of the three row shapes.
fn paint_row_highlight(
    bg_idx: egui::layers::ShapeIdx,
    row: egui::Rect,
    fill: egui::Color32,
    ui: &egui::Ui,
) {
    ui.painter().set(
        bg_idx,
        egui::Shape::rect_filled(
            row.expand2(egui::vec2(notedeck::tokens::SPACING_XS, 1.0)),
            notedeck::tokens::RADIUS_SM,
            fill,
        ),
    );
}

/// Render a responded (Allowed/Denied) permission request as a collapsible
/// row: a status header with a disclosure chevron that expands to reveal what
/// was actually allowed/denied (the edit diff, or the bash command / argument),
/// so the operation stays inspectable after the fact without cluttering chat.
fn responded_permission_ui(
    request: &PermissionRequest,
    label: &str,
    label_color: egui::Color32,
    inner_margin: f32,
    corner_radius: f32,
    nav: &mut BlockNav,
    ui: &mut egui::Ui,
) {
    // The row is only expandable if there's an operation to reveal. Edit/Write
    // carry a file_path, so `detail.is_some()` already covers them.
    let detail = permission_operation_detail(&request.tool_input);
    let expandable = detail.is_some();

    egui::Frame::new()
        .fill(ui.visuals().widgets.noninteractive.bg_fill)
        .inner_margin(inner_margin)
        .corner_radius(corner_radius)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                // Persisted disclosure state, keyed by request id. Auto-accepted
                // rows (runtime allowlist / auto mode) start expanded so the
                // user can review what they never approved up front; manually
                // approved rows start collapsed. A user toggle overrides either.
                let expand_id = ui.scope_id().with(("responded_perm", request.id));
                let block = expandable.then(|| nav.register(expand_id, request.auto_accepted, ui));
                let mut expanded = block.is_some_and(|block| block.expanded);

                // Same reserved slot as the tool-result rows, for the same
                // reason: the block cursor paints behind the header once its
                // rect is known.
                let bg_idx = ui.painter().add(egui::Shape::Noop);
                let full_width = ui.available_width();

                let header = responded_permission_header_ui(
                    request,
                    label,
                    label_color,
                    expanded,
                    expandable,
                    ui,
                );

                if let Some(block) = block {
                    let row_rect = egui::Rect::from_min_size(
                        header.rect.min,
                        egui::vec2(full_width, header.rect.height()),
                    );
                    if block.is_cursor {
                        paint_row_highlight(bg_idx, row_rect, block_cursor_fill(ui), ui);
                    }
                    if block.scroll_into_view {
                        header.scroll_to_me(Some(egui::Align::Center));
                    }
                }

                // Clicking anywhere on the header toggles the disclosure.
                if expandable && header.clicked() {
                    expanded = !expanded;
                    ui.data_mut(|d| d.insert_temp(expand_id, expanded));
                }

                if !expanded {
                    return;
                }

                // Prefer a rich diff for file edits; otherwise show the raw
                // operation (bash command / single argument) in monospace.
                if let Some(file_update) =
                    FileUpdate::from_tool_call(&request.tool_name, &request.tool_input)
                {
                    diff::file_path_header(&file_update, ui);
                    diff::file_update_ui(&file_update, false, ui);
                } else if let Some(detail) = detail {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(detail)
                                .monospace()
                                .size(11.0)
                                .color(ui.visuals().weak_text_color()),
                        )
                        .wrap_mode(egui::TextWrapMode::Wrap),
                    );
                }
            });
        });
}

/// Header row for a responded permission: optional disclosure chevron, the
/// status label (Allowed/Denied), and the tool name. Returns a click response
/// covering the whole row so the caller can toggle the disclosure.
fn responded_permission_header_ui(
    request: &PermissionRequest,
    label: &str,
    label_color: egui::Color32,
    expanded: bool,
    expandable: bool,
    ui: &mut egui::Ui,
) -> egui::Response {
    let header = ui.horizontal(|ui| {
        if expandable {
            notedeck_ui::header::disclosure_chevron(
                ui,
                expanded,
                egui::Stroke::new(1.5_f32, ui.visuals().weak_text_color()),
            );
        }
        ui.label(egui::RichText::new(label).color(label_color).strong());
        ui.label(egui::RichText::new(&request.tool_name).color(ui.visuals().text_color()));
    });

    if !expandable {
        return header.response;
    }

    // Re-sensing the horizontal layout's response (`.interact(Sense::click())`)
    // doesn't reliably register hover/click, so allocate a dedicated clickable
    // region over the row's rect with a stable id keyed by the request.
    let click_id = ui.scope_id().with(("responded_perm_header", request.id));
    ui.interact(header.response.rect, click_id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
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
        PermissionMessageState::TentativeAllowAlways => {
            if ui
                .link(
                    egui::RichText::new(format!("✓ Will {accept_label} always"))
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

/// Format a wall-clock unix-seconds timestamp as a relative time string
/// (e.g. "just now", "3m ago"). `saturating_sub` renders a future-dated
/// timestamp (clock skew) as "just now" rather than underflowing.
fn format_relative_time(unix_secs: u64) -> String {
    let elapsed = crate::session::now_unix().saturating_sub(unix_secs);
    if elapsed < 60 {
        "just now".to_string()
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 86400 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86400)
    }
}

/// Renders the status bar containing git status, run button, and toggle badges.
impl DaveUi<'_> {
    fn status_bar_ui(
        &mut self,
        i18n: &mut notedeck::Localization,
        ui: &mut egui::Ui,
    ) -> Option<DaveAction> {
        let is_remote = self.flags.contains(DaveUiFlags::IsRemote);
        let is_agentic = self.ai_mode == AiMode::Agentic;
        let cwd = self.details.and_then(|d| d.cwd.as_deref());

        let snapshot = self
            .git_status
            .as_deref()
            .and_then(git_status_ui::StatusSnapshot::from_cache);

        ui.vertical(|ui| {
            let bar_action = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Run config buttons — leftmost elements, local sessions only
                    let mut action = if !is_remote {
                        let a = cwd.and_then(|p| {
                            run_ui::run_configs_ui(self.run_configs, self.running_config_ids, p, ui)
                        });
                        if cwd.is_some() {
                            ui.separator();
                        }
                        a
                    } else {
                        None
                    };

                    if let Some(gs) = self.git_status.as_deref_mut() {
                        git_status_ui::git_status_content_ui(gs, &snapshot, ui);
                    }

                    let right = ui
                        .with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let badge_action = if is_agentic {
                                toggle_badges_ui(
                                    ui,
                                    self.permission_mode,
                                    self.flags.contains(DaveUiFlags::AutoStealFocus),
                                    self.focus_queue_info,
                                )
                            } else {
                                None
                            };
                            if is_agentic {
                                if let Some(ts) = self.last_activity {
                                    ui.label(
                                        egui::RichText::new(format_relative_time(ts))
                                            .size(10.0)
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                // The inspector's usage history, which both a
                                // local stream and a host's snapshot fill.
                                let usage = self.inspector.as_ref().map(|i| &i.inspector.usage);
                                if usage_meter_ui(usage, self.context_window, ui).clicked() {
                                    return Some(DaveAction::OpenInspector(
                                        InspectorSection::Usage,
                                    ));
                                }
                                if self.rate_limit_pill_ui(i18n, ui) {
                                    return Some(DaveAction::OpenInspector(
                                        InspectorSection::RateLimits,
                                    ));
                                }
                                if self.waiting_pill_ui(i18n, ui) {
                                    return Some(DaveAction::OpenInspector(
                                        InspectorSection::Background,
                                    ));
                                }
                            }
                            badge_action
                        })
                        .inner;

                    action = action.or(right);
                    action
                })
                .inner;

            if let Some(gs) = self.git_status.as_deref() {
                git_status_ui::git_expanded_files_ui(gs, &snapshot, ui);
            }

            bar_action
        })
        .inner
    }
}

impl DaveUi<'_> {
    /// The status bar's rate-limit pill, shown while the account is near or
    /// over a limit. True when it was clicked.
    fn rate_limit_pill_ui(&mut self, i18n: &mut notedeck::Localization, ui: &mut egui::Ui) -> bool {
        let (Some(info), Some(inspected)) = (self.rate_limit.as_ref(), self.inspector.as_mut())
        else {
            return false;
        };
        let labels = &mut inspected.inspector.rate_limit_labels;
        rate_limit_pill_ui(info, labels, i18n, ui).is_some_and(|resp| resp.clicked())
    }

    /// The status bar's Waiting pill, shown while the session is parked on
    /// background work that will resume it. True when it was clicked.
    fn waiting_pill_ui(&mut self, i18n: &mut notedeck::Localization, ui: &mut egui::Ui) -> bool {
        let Some(inspected) = self.inspector.as_mut() else {
            return false;
        };
        if inspected.status != crate::agent_status::AgentStatus::Waiting {
            return false;
        }
        let memo = &mut inspected.inspector.background.waiting_pill;
        waiting_pill_ui(inspected.waiting_on, memo, i18n, ui).clicked()
    }
}

/// Format a token count in a compact human-readable form (e.g. "45K", "1.2M")
/// The block id a subagent's chat row registers under. Not scoped to the
/// chat's `Ui`, so the Subagents card can name the row it jumps to; the task
/// id (a tool_use id) is unique on its own.
pub(crate) fn subagent_block_id(task_id: &str) -> egui::Id {
    egui::Id::unique("dave_subagent_block").with(task_id)
}

pub(crate) fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// The status bar's usage meter (cost, tokens and fill bar) as one clickable
/// group: a click opens the Details tab at its Usage card.
fn usage_meter_ui(
    usage: Option<&crate::inspector::UsageHistory>,
    context_window: u64,
    ui: &mut egui::Ui,
) -> egui::Response {
    let meter = ui.scope(|ui| usage_bar_ui(usage, context_window, ui));
    ui.interact(
        meter.response.rect,
        ui.scope_id().with("dave_usage_meter"),
        egui::Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A fill bar's colour by how full it is: green under half, yellow under
/// 80%, red past that.
pub(crate) fn fill_color(fraction: f32) -> egui::Color32 {
    if fraction < 0.5 {
        egui::Color32::from_rgb(100, 180, 100)
    } else if fraction < 0.8 {
        egui::Color32::from_rgb(200, 180, 60)
    } else {
        egui::Color32::from_rgb(200, 80, 80)
    }
}

/// Renders the usage fill bar showing context window consumption: the
/// latest request's context and the session's cost, from `usage`. Draws
/// nothing until a request has been counted.
fn usage_bar_ui(
    usage: Option<&crate::inspector::UsageHistory>,
    context_window: u64,
    ui: &mut egui::Ui,
) {
    let Some(usage) = usage else {
        return;
    };
    let total = usage.current().map(|t| t.context()).unwrap_or(0);
    if total == 0 {
        return;
    }
    let fraction = (total as f64 / context_window as f64).min(1.0) as f32;

    let bar_color = fill_color(fraction);

    let weak = ui.visuals().weak_text_color();

    // Cost label
    if let Some(cost) = usage.total_cost() {
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

/// The status-bar badge for a permission mode: its label and colour, matching
/// Claude Code's own (Manual neutral, Plan blue, Accept Edits purple, Auto
/// yellow). Bypass is never reachable from the UI but is drawn red.
pub(crate) fn permission_mode_badge(mode: PermissionMode) -> StatusBadge<'static> {
    let (label, variant) = match mode {
        PermissionMode::Default => ("MANUAL", BadgeVariant::Default),
        PermissionMode::Plan => ("PLAN", BadgeVariant::Info),
        PermissionMode::AcceptEdits => ("ACCEPT EDITS", BadgeVariant::Purple),
        PermissionMode::Auto => ("AUTO", BadgeVariant::Warning),
        PermissionMode::BypassPermissions => ("BYPASS", BadgeVariant::Destructive),
    };
    StatusBadge::new(label).variant(variant)
}

/// Render clickable permission mode and FOCUS toggle badges. Returns an action if clicked.
fn toggle_badges_ui(
    ui: &mut egui::Ui,
    permission_mode: PermissionMode,
    auto_steal_focus: bool,
    focus_queue_info: Option<FocusQueueInfo>,
) -> Option<DaveAction> {
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let is_narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut action = None;

    // NEXT badge for focus queue navigation (narrow/mobile only, rendered first = rightmost)
    if is_narrow {
        if let Some(FocusQueueInfo { priority, .. }) = focus_queue_info {
            let variant = match priority {
                FocusPriority::NeedsInput => super::badge::BadgeVariant::Warning,
                FocusPriority::Error => super::badge::BadgeVariant::Destructive,
                FocusPriority::Done => super::badge::BadgeVariant::Info,
            };
            // A proper chevron icon with a comfortable tap target so it's easy
            // to hit on narrow/touch layouts (the old ▶ glyph was tiny).
            let mut next_badge = super::badge::StatusBadge::new("Next focus")
                .icon(super::badge::BadgeIcon::ChevronRight)
                .variant(variant)
                .min_size(egui::vec2(40.0, 28.0));
            if ctrl_held {
                next_badge = next_badge.keybind("N");
            }
            if next_badge
                .show(ui)
                .on_hover_text("Next in focus queue (Ctrl+N)")
                .clicked()
            {
                action = Some(DaveAction::FocusQueueNext);
            }
        }
    }

    // AUTO badge (rendered first in right-to-left, so it appears rightmost)
    let mut focus_badge = super::badge::StatusBadge::new("FOCUS").variant(if auto_steal_focus {
        super::badge::BadgeVariant::Info
    } else {
        super::badge::BadgeVariant::Default
    });
    if ctrl_held {
        focus_badge = focus_badge.keybind("\\");
    }
    if focus_badge
        .show(ui)
        .on_hover_text("Click or Ctrl+\\ to toggle auto-focus mode")
        .clicked()
    {
        action = Some(DaveAction::ToggleAutoSteal);
    }

    // Permission mode badge. Either click opens a menu that sets a mode; there
    // is no cycle to step through by accident. In normal mode `m m`, `m p`,
    // `m e` and `m a` set them by name.
    let mode_resp = permission_mode_badge(permission_mode)
        .show(ui)
        .on_hover_text("Permission mode · click to pick (normal mode: mm / mp / me / ma)");
    notedeck_ui::context_menu::click_menu(&mode_resp, |ui| {
        ui.label(egui::RichText::new("Permission mode").small().weak());
        if ui.button("Manual").clicked() {
            action = Some(DaveAction::SetPermissionMode(PermissionMode::Default));
            ui.close();
        }
        if ui.button("Plan").clicked() {
            action = Some(DaveAction::SetPermissionMode(PermissionMode::Plan));
            ui.close();
        }
        if ui.button("Accept Edits").clicked() {
            action = Some(DaveAction::SetPermissionMode(PermissionMode::AcceptEdits));
            ui.close();
        }
        if ui
            .button("Auto")
            .on_hover_text(
                "Auto-approve tool calls that pass Claude Code's safety classifier; \
                 dangerous ones still prompt.",
            )
            .clicked()
        {
            action = Some(DaveAction::SetPermissionMode(PermissionMode::Auto));
            ui.close();
        }
    });

    // COMPACT badge
    let compact_badge =
        super::badge::StatusBadge::new("COMPACT").variant(super::badge::BadgeVariant::Default);
    if compact_badge
        .show(ui)
        .on_hover_text("Click to compact context")
        .clicked()
    {
        action = Some(DaveAction::Compact);
    }

    action
}

/// Full CWD string from the session header, for tooltip display on hover.
pub(crate) struct HeaderCwd {
    full: Option<String>,
}

/// What a subagent's header adds to its subtitle: the model, cwd and mode
/// shown are its parent's.
const SUBAGENT_INHERITS: &str = "inherited from its parent";

/// Font of the header's title line.
const HEADER_TITLE_SIZE: f32 = 13.0;

/// Draw the chat header: the backend icon, the session's title, and a
/// subtitle of its model and cwd.
///
/// With `refs`, a title holding a reference (autowork titles a session with
/// its card's `headway:<board>/<word-id>`) draws the reference as its inline
/// chip, through the session list's [`title_with_refs_ui`], so the header
/// shows the card's live status and title rather than the raw ref. A click
/// on the chip opens the card in headway.
pub(crate) fn session_header_ui(
    ui: &mut egui::Ui,
    details: &SessionDetails,
    backend_type: BackendType,
    is_subagent: bool,
    refs: Option<&mut TitleRefs<'_, '_>>,
) -> HeaderCwd {
    let mut header_cwd = HeaderCwd { full: None };

    ui.horizontal(|ui| {
        // Backend icon
        if backend_type.is_agentic() {
            let icon = crate::ui::backend_icon(backend_type).max_height(16.0);
            ui.add(icon);
        }

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;

            header_title_ui(ui, details.display_title(), refs);

            // Subtitle line: model + cwd
            let model_text = details.display_model();
            let cwd_text = details.cwd.as_ref().map(|cwd| {
                let cwd_display = if details.home_dir.is_empty() {
                    crate::path_utils::abbreviate_path(cwd)
                } else {
                    crate::path_utils::abbreviate_with_home(cwd, &details.home_dir)
                };
                if details.hostname.is_empty() {
                    cwd_display
                } else {
                    format!("{}:{}", details.hostname, cwd_display)
                }
            });

            let subtitle = match (model_text, &cwd_text) {
                (Some(model), Some(cwd)) => Some(format!("{} \u{00B7} {}", model, cwd)),
                (Some(model), None) => Some(model.to_string()),
                (None, Some(cwd)) => Some(cwd.clone()),
                (None, None) => None,
            };
            let subtitle = match subtitle {
                Some(text) if is_subagent => Some(format!("{text} \u{00B7} {SUBAGENT_INHERITS}")),
                None if is_subagent => Some(SUBAGENT_INHERITS.to_string()),
                other => other,
            };

            if let Some(text) = subtitle {
                ui.add(
                    egui::Label::new(egui::RichText::new(text).monospace().size(10.0).weak())
                        .wrap_mode(egui::TextWrapMode::Truncate),
                );

                header_cwd.full = cwd_text;
            }
        });
    });

    header_cwd
}

/// The header's title line: `title` with its references drawn as their
/// chips when `refs` is given and the title holds one, else plain text,
/// end-truncated.
fn header_title_ui(ui: &mut egui::Ui, title: &str, refs: Option<&mut TitleRefs<'_, '_>>) {
    let refs = refs.filter(|refs| {
        refs.note
            .registries
            .reference_parsers
            .find_next(title)
            .is_some()
    });
    let Some(refs) = refs else {
        ui.add(
            egui::Label::new(egui::RichText::new(title).size(HEADER_TITLE_SIZE))
                .wrap_mode(egui::TextWrapMode::Truncate),
        );
        return;
    };
    let font_id = egui::FontId::proportional(HEADER_TITLE_SIZE);
    let height = ui.fonts_mut(|f| f.row_height(&font_id));
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    title_with_refs_ui(
        ui,
        refs,
        title,
        rect.min,
        rect.width(),
        &font_id,
        // Unlike a row, the header has no click of its own: the chip opens
        // its card.
        notedeck::Interaction::Open,
    );
}

#[cfg(test)]
mod tests {
    use super::{toggle_badges_ui, BlockNav, DaveAction, DaveUi};
    use crate::config::AiMode;
    use crate::messages::{
        PermissionRequest, PermissionResponse, PermissionResponseType, QuestionAnswer,
        SubagentInfo, SubagentStatus, SystemMessage,
    };
    use claude_agent_sdk_rs::PermissionMode;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use notedeck::test_harness::PressKey;
    use serde_json::json;
    use std::collections::HashMap;
    use uuid::Uuid;

    /// Harness state for exercising the permission-mode badge + its right-click
    /// menu in isolation.
    struct BadgeHarnessState {
        mode: PermissionMode,
        action: Option<DaveAction>,
    }

    fn badge_harness(mode: PermissionMode) -> Harness<'static, BadgeHarnessState> {
        notedeck::test_harness::lenient_builder().build_ui_state(
            |ui, state: &mut BadgeHarnessState| {
                if let Some(action) = toggle_badges_ui(ui, state.mode, false, None) {
                    state.action = Some(action);
                }
            },
            BadgeHarnessState { mode, action: None },
        )
    }

    /// Right-click the mode badge and click the center of it to open the menu.
    fn right_click(harness: &mut Harness<'static, BadgeHarnessState>, label: &str) {
        let bounds = harness
            .get_by_label(label)
            .accesskit_node()
            .raw_bounds()
            .expect("badge bounds");
        let center = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        // The menu's first frame is an invisible sizing pass; settle so its
        // items are laid out and reachable.
        harness.run();
    }

    #[test]
    fn mode_badge_left_click_opens_the_menu() {
        // Left-clicking the badge (shows "MANUAL" in Default) changes nothing
        // by itself: it opens the same menu the right click does.
        let mut harness = badge_harness(PermissionMode::Default);
        harness.run();
        harness.get_by_label("MANUAL").click_accesskit();
        // The menu's first frame is an invisible sizing pass.
        harness.run();
        assert!(
            harness.state().action.is_none(),
            "a click alone must not change the mode"
        );

        harness.get_by_label("Plan").click_accesskit();
        harness.run();
        assert!(
            matches!(
                harness.state().action,
                Some(DaveAction::SetPermissionMode(PermissionMode::Plan))
            ),
            "the menu's Plan item should set Plan"
        );
    }

    #[test]
    fn auto_mode_reachable_via_right_click_menu() {
        let mut harness = badge_harness(PermissionMode::Default);
        harness.run();

        // Open the mode badge's context menu and pick "Auto".
        right_click(&mut harness, "MANUAL");
        harness.get_by_label("Auto").click_accesskit();
        harness.run();

        assert!(
            matches!(
                harness.state().action,
                Some(DaveAction::SetPermissionMode(PermissionMode::Auto))
            ),
            "the menu's Auto item should set the Auto permission mode"
        );
    }

    #[test]
    fn auto_mode_shows_auto_badge() {
        // In Auto permission mode the badge reads "AUTO".
        let mut harness = badge_harness(PermissionMode::Auto);
        harness.run();
        // Queryable by its text means the label rendered.
        let _ = harness.get_by_label("AUTO");
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_permission_mode_badges() {
        let modes = [
            ("Manual", PermissionMode::Default),
            ("Plan", PermissionMode::Plan),
            ("Accept Edits", PermissionMode::AcceptEdits),
            ("Auto", PermissionMode::Auto),
        ];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(360.0, 250.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                for (name, mode) in modes {
                    ui.label(name);
                    ui.horizontal(|ui| {
                        let _ = toggle_badges_ui(ui, mode, false, None);
                    });
                    ui.add_space(4.0);
                }
            });
        harness.run();
        harness.snapshot("permission_mode_badges");
    }

    struct PermissionUiHarnessState {
        request: PermissionRequest,
        input: String,
        focus_requested: bool,
        block_nav: BlockNav,
        question_answers: HashMap<Uuid, Vec<QuestionAnswer>>,
        question_index: HashMap<Uuid, usize>,
        action: Option<DaveAction>,
    }

    impl PermissionUiHarnessState {
        fn new(request: PermissionRequest) -> Self {
            Self {
                request,
                input: String::new(),
                focus_requested: false,
                block_nav: BlockNav::default(),
                question_answers: HashMap::new(),
                question_index: HashMap::new(),
                action: None,
            }
        }
    }

    #[test]
    fn approval_prompt_ui_allows_request() {
        let expected_request_id = Uuid::new_v4();
        let request = PermissionRequest::pending(
            expected_request_id,
            "SaveIssue".to_string(),
            json!({
                "questions": [{
                    "header": "Approve app tool call?",
                    "question": "Allow this action?"
                }]
            }),
        );

        let mut harness = notedeck::test_harness::lenient_builder().build_ui_state(
            |ui, state: &mut PermissionUiHarnessState| {
                let mut dave_ui = DaveUi::new(
                    false,
                    1,
                    &[],
                    &mut state.input,
                    &mut state.focus_requested,
                    &mut state.block_nav,
                    AiMode::Agentic,
                );
                if let Some(action) = dave_ui.permission_request_ui(&state.request, None, ui) {
                    state.action = Some(action);
                }
            },
            PermissionUiHarnessState::new(request),
        );

        harness.run();
        harness.get_by_label("SaveIssue");
        harness.get_by_label("Allow this action?");
        harness.get_by_label("Allow").click_accesskit();
        harness.run();

        match harness.state().action.as_ref() {
            Some(DaveAction::PermissionResponse {
                request_id,
                response,
            }) => {
                assert_eq!(*request_id, expected_request_id);
                assert!(matches!(
                    response,
                    PermissionResponse::Allow { message: None }
                ));
            }
            other => panic!("expected PermissionResponse action, got {:?}", other),
        }
    }

    #[test]
    fn permission_request_ui_exit_button_emits_exit_tool_call_action() {
        let expected_request_id = Uuid::new_v4();
        let request = PermissionRequest::pending(
            expected_request_id,
            "Bash".to_string(),
            json!({
                "command": "rm -rf /tmp/scratch"
            }),
        );

        let mut harness = notedeck::test_harness::lenient_builder().build_ui_state(
            |ui, state: &mut PermissionUiHarnessState| {
                let mut dave_ui = DaveUi::new(
                    false,
                    1,
                    &[],
                    &mut state.input,
                    &mut state.focus_requested,
                    &mut state.block_nav,
                    AiMode::Agentic,
                );
                if let Some(action) = dave_ui.permission_request_ui(&state.request, None, ui) {
                    state.action = Some(action);
                }
            },
            PermissionUiHarnessState::new(request),
        );

        harness.run();
        harness.get_by_label("Exit").click_accesskit();
        harness.run();

        match harness.state().action.as_ref() {
            Some(DaveAction::ExitToolCall { request_id }) => {
                assert_eq!(*request_id, expected_request_id);
            }
            other => panic!("expected ExitToolCall action, got {:?}", other),
        }
    }

    #[test]
    fn question_set_prompt_ui_submits_selected_answer() {
        let expected_request_id = Uuid::new_v4();
        let request = PermissionRequest::pending(
            expected_request_id,
            "AskUserQuestion".to_string(),
            json!({
                "questions": [{
                    "header": "Theme",
                    "question": "Pick a theme",
                    "multiSelect": false,
                    "options": [{
                        "label": "Light",
                        "description": "Bright background"
                    }]
                }]
            }),
        );

        let mut harness = notedeck::test_harness::lenient_builder().build_ui_state(
            |ui, state: &mut PermissionUiHarnessState| {
                let mut dave_ui = DaveUi::new(
                    false,
                    1,
                    &[],
                    &mut state.input,
                    &mut state.focus_requested,
                    &mut state.block_nav,
                    AiMode::Agentic,
                )
                .question_answers(&mut state.question_answers)
                .question_index(&mut state.question_index);
                if let Some(action) = dave_ui.permission_request_ui(&state.request, None, ui) {
                    state.action = Some(action);
                }
            },
            PermissionUiHarnessState::new(request),
        );

        harness.run();
        harness.get_by_label("Pick a theme");
        // The option's text, and its radio button named after it.
        harness.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Light");
        harness.get_by_label("Bright background");
        harness.press_key(egui::Key::Num1);
        harness.step();
        harness.get_by_label("Submit").click_accesskit();
        harness.run();

        match harness.state().action.as_ref() {
            Some(DaveAction::QuestionResponse {
                request_id,
                answers,
            }) => {
                assert_eq!(*request_id, expected_request_id);
                assert_eq!(answers.len(), 1);
                assert_eq!(answers[0].selected, vec![0]);
                assert_eq!(answers[0].other_text, None);
            }
            other => panic!("expected QuestionResponse action, got {:?}", other),
        }
    }

    /// Build a harness that renders a single (already-responded) permission
    /// request through the real `permission_request_ui`.
    fn responded_permission_harness(
        request: PermissionRequest,
    ) -> Harness<'static, PermissionUiHarnessState> {
        notedeck::test_harness::lenient_builder().build_ui_state(
            |ui, state: &mut PermissionUiHarnessState| {
                let mut dave_ui = DaveUi::new(
                    false,
                    1,
                    &[],
                    &mut state.input,
                    &mut state.focus_requested,
                    &mut state.block_nav,
                    AiMode::Agentic,
                );
                if let Some(action) = dave_ui.permission_request_ui(&state.request, None, ui) {
                    state.action = Some(action);
                }
            },
            PermissionUiHarnessState::new(request),
        )
    }

    /// Primary-click the center of a labelled node. `.click()` on a plain
    /// egui `Label` is a no-op in accesskit (labels expose no default action),
    /// so a disclosure driven by `Response::interact(Sense::click())` must be
    /// exercised with a real positional pointer event instead.
    fn click_label(harness: &mut Harness<'static, PermissionUiHarnessState>, label: &str) {
        let bounds = harness
            .get_by_label(label)
            .accesskit_node()
            .raw_bounds()
            .expect("label bounds");
        let center = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();
    }

    #[test]
    fn allowed_bash_widget_reveals_command_when_expanded() {
        // After accepting, the row is collapsed to "Allowed  Bash"; expanding
        // it must reveal *what* was allowed — the bash command.
        let request = PermissionRequest::new(
            Uuid::new_v4(),
            "Bash".to_string(),
            json!({ "command": "cargo test --all" }),
            None,
            Some(PermissionResponseType::Allowed),
            None,
        );
        let mut harness = responded_permission_harness(request);
        harness.run();

        harness.get_by_label("Allowed");
        harness.get_by_label("Bash");
        // Collapsed by default: the command is hidden until disclosed.
        assert!(harness.query_by_label("cargo test --all").is_none());

        // Clicking the header expands the row.
        click_label(&mut harness, "Bash");
        harness.run();
        harness.get_by_label("cargo test --all");
    }

    #[test]
    fn denied_edit_widget_reveals_file_when_expanded() {
        // Likewise for a denied Edit: expanding reveals the edited file path.
        let request = PermissionRequest::new(
            Uuid::new_v4(),
            "Edit".to_string(),
            json!({
                "file_path": "crates/notedeck_dave/src/lib.rs",
                "old_string": "a",
                "new_string": "b",
            }),
            None,
            Some(PermissionResponseType::Denied),
            None,
        );
        let mut harness = responded_permission_harness(request);
        harness.run();

        harness.get_by_label("Denied");
        // Collapsed by default: the path is hidden until disclosed.
        assert!(harness
            .query_by_label("crates/notedeck_dave/src/lib.rs")
            .is_none());

        // Clicking the header expands the row to show the file + diff.
        click_label(&mut harness, "Denied");
        harness.run();
        harness.get_by_label("crates/notedeck_dave/src/lib.rs");
    }

    #[test]
    fn auto_accepted_widget_starts_expanded() {
        // An auto-accepted row (runtime allowlist / auto mode) was never
        // reviewed up front, so it starts EXPANDED — the command is visible
        // without any click.
        let request = PermissionRequest::pending(
            Uuid::new_v4(),
            "Bash".to_string(),
            json!({ "command": "cargo test --all" }),
        )
        .auto_accept();
        assert!(request.auto_accepted);
        let mut harness = responded_permission_harness(request);
        harness.run();

        harness.get_by_label("Allowed");
        harness.get_by_label("Bash");
        // Expanded by default: the command is shown without clicking.
        harness.get_by_label("cargo test --all");
    }

    /// Visualize the responded permission widgets. Ignored by default; render
    /// with `scripts/snapshot-test snapshot_responded_permission_widgets`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_responded_permission_widgets() {
        let requests = vec![
            PermissionRequest::new(
                Uuid::new_v4(),
                "Bash".to_string(),
                json!({ "command": "cargo test --all -- --nocapture" }),
                None,
                Some(PermissionResponseType::Allowed),
                None,
            ),
            PermissionRequest::new(
                Uuid::new_v4(),
                "Edit".to_string(),
                json!({
                    "file_path": "crates/notedeck_dave/src/ui/badge.rs",
                    "old_string": "a",
                    "new_string": "b",
                }),
                None,
                Some(PermissionResponseType::Denied),
                None,
            ),
        ];

        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 180.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let mut input = String::new();
                let mut focus = false;
                let mut nav = BlockNav::default();
                for request in &requests {
                    let mut dave_ui = DaveUi::new(
                        false,
                        1,
                        &[],
                        &mut input,
                        &mut focus,
                        &mut nav,
                        AiMode::Agentic,
                    );
                    dave_ui.permission_request_ui(request, None, ui);
                    ui.add_space(8.0);
                }
            });

        harness.run();
        harness.snapshot("responded_permission_widgets");
    }

    /// Visualize the responded permission widgets in their *expanded* state —
    /// the Bash command revealed inline and the denied Edit's file diff. Render
    /// with `scripts/snapshot-test snapshot_responded_permission_widgets_expanded`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_responded_permission_widgets_expanded() {
        let requests = vec![
            PermissionRequest::new(
                Uuid::new_v4(),
                "Bash".to_string(),
                json!({ "command": "cargo test --all -- --nocapture" }),
                None,
                Some(PermissionResponseType::Allowed),
                None,
            ),
            PermissionRequest::new(
                Uuid::new_v4(),
                "Edit".to_string(),
                json!({
                    "file_path": "crates/notedeck_dave/src/ui/badge.rs",
                    "old_string": "let expandable = false;",
                    "new_string": "let expandable = detail.is_some();",
                }),
                None,
                Some(PermissionResponseType::Denied),
                None,
            ),
        ];

        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 320.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let mut input = String::new();
                let mut focus = false;
                let mut nav = BlockNav::default();
                for request in &requests {
                    let mut dave_ui = DaveUi::new(
                        false,
                        1,
                        &[],
                        &mut input,
                        &mut focus,
                        &mut nav,
                        AiMode::Agentic,
                    );
                    dave_ui.permission_request_ui(request, None, ui);
                    ui.add_space(8.0);
                }
            });

        harness.run();
        // "Allowed" and "Denied" are each unique here, so clicking those status
        // labels expands each row's disclosure. Labels are no-op-clickable in
        // accesskit, so dispatch a real positional primary click.
        for status in ["Allowed", "Denied"] {
            let bounds = harness
                .get_by_label(status)
                .accesskit_node()
                .raw_bounds()
                .expect("status label bounds");
            let center = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(center));
            for pressed in [true, false] {
                harness.input_mut().events.push(egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
        }
        harness.run();
        harness.snapshot("responded_permission_widgets_expanded");
    }

    /// Build the mixed tool-result list the snapshots exercise: a Bash result
    /// carrying stdout, a Bash result with no output, and a plain Read summary.
    fn executed_tool_fixtures() -> Vec<crate::messages::ExecutedTool> {
        vec![
            crate::messages::ExecutedTool {
                tool_name: "Bash".to_string(),
                summary: "`ls -la crates`".to_string(),
                output: Some(
                    "total 24\ndrwxr-xr-x  notedeck\ndrwxr-xr-x  notedeck_dave\n-rw-r--r--  Cargo.toml"
                        .into(),
                ),
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
            crate::messages::ExecutedTool {
                tool_name: "Bash".to_string(),
                summary: "`true`".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
            crate::messages::ExecutedTool {
                tool_name: "Read".to_string(),
                summary: "lib.rs (128 lines)".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
        ]
    }

    /// An executed edit (auto mode: the CLI approved it, so no permission row
    /// ever showed the diff) starts EXPANDED even when the diff is large — the
    /// file header and diff render without a click.
    #[test]
    fn executed_large_edit_starts_expanded() {
        let old_string: String = (0..20).map(|i| format!("old line {i}\n")).collect();
        let new_string: String = (0..20).map(|i| format!("new line {i}\n")).collect();
        let file_update = crate::file_update::FileUpdate::from_tool_call(
            "Edit",
            &json!({
                "file_path": "crates/notedeck_dave/src/lib.rs",
                "old_string": old_string,
                "new_string": new_string,
            }),
        )
        .expect("edit file update");
        assert!(
            file_update.diff_lines().len() >= 10,
            "fixture must exceed the old small-diff threshold"
        );
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Edit".to_string(),
            summary: "lib.rs".to_string(),
            output: None,
            parent_task_id: None,
            file_update: Some(file_update),
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 800.0))
            .build_ui(move |ui| {
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                }
            });
        harness.run();

        // The file header only renders when the diff body is expanded.
        harness.get_by_label("crates/notedeck_dave/src/lib.rs");
    }

    /// Clicking anywhere on a collapsible tool-result summary line — not just
    /// the chevron — toggles its body. Guards headway:dave/retire-garage-accuse:
    /// the click target spans the *full width* of the row, so a click in the
    /// empty space well past the summary text (nowhere near the chevron) still
    /// toggles it. Bash-output rows start *expanded*, so the click here
    /// collapses the body.
    #[test]
    fn clicking_tool_summary_line_toggles_body() {
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: "`ls -la crates`".to_string(),
            output: Some("total 24\nCargo.toml".into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 120.0))
            .build_ui(move |ui| {
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                }
            });
        harness.run();

        // Expanded by default: the body renders the *bare* command (backticks
        // stripped), so its presence proves the row starts open without a click.
        harness.get_by_label("ls -la crates");

        // Click the empty space to the far right of the summary text, well past
        // both the chevron and the command, to prove the whole line is the hit
        // target rather than a tiny region around the chevron.
        let bounds = harness
            .get_by_label("Bash")
            .accesskit_node()
            .raw_bounds()
            .expect("tool-name label bounds");
        let y = ((bounds.y0 + bounds.y1) / 2.0) as f32;
        let pos = egui::pos2(400.0, y);
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();
        harness.run();

        // Body hidden: the click collapsed the row, so the bare command is gone.
        assert!(harness.query_by_label("ls -la crates").is_none());
    }

    /// Visualize the tool-result rows in their default state: the
    /// Bash-with-output row shows a ▼ disclosure with its body expanded, the
    /// others render as plain one-liners. Render with
    /// The in-flight "running" rows: a spinner in place of the disclosure arrow,
    /// then the tool name + call-time summary, matching the completed row's
    /// subdued monospace styling so the swap-in-place on completion is seamless.
    /// Run via `scripts/snapshot-test snapshot_running_tool_rows`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_running_tool_rows() {
        let running = vec![
            crate::messages::RunningTool {
                tool_use_id: "t1".to_string(),
                tool_name: "Bash".to_string(),
                summary: "`cargo test --all`".to_string(),
            },
            crate::messages::RunningTool {
                tool_use_id: "t2".to_string(),
                tool_name: "Read".to_string(),
                summary: "lib.rs (0 lines)".to_string(),
            },
            crate::messages::RunningTool {
                tool_use_id: "t3".to_string(),
                tool_name: "Grep".to_string(),
                summary: "'fn handle_stream_message'".to_string(),
            },
        ];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 120.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                for rt in &running {
                    DaveUi::tool_running_ui(rt, ui);
                    ui.add_space(4.0);
                }
            });

        harness.run();
        harness.snapshot("running_tool_rows");
    }

    /// What `/context` replies with: a heading and a markdown table, which the
    /// notice row draws through the markdown renderer.
    const CONTEXT_REPLY: &str = "## Context Usage\n\n\
| Category      | Tokens | Percentage |\n\
|---------------|--------|------------|\n\
| System prompt | 3.1k   | 1.6%       |\n\
| Messages      | 41.2k  | 20.6%      |\n\
| Free space    | 155.7k | 77.8%      |";

    /// Draw `text` as a `system` row in a harness, agentic or not.
    fn system_row_harness(text: &'static str, is_agentic: bool) -> Harness<'static> {
        let system = SystemMessage::from(text);
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 200.0))
            .build_ui(move |ui| DaveUi::system_message_ui(&system, is_agentic, ui));
        harness.run();
        harness
    }

    /// A chat-mode session's `system` row is its system prompt, which the
    /// chat never shows.
    #[test]
    fn system_row_hidden_in_chat_mode() {
        let harness = system_row_harness("You are an AI agent called Dave", false);
        assert!(harness.query_by_label_contains("AI agent").is_none());
    }

    /// In an agentic session a `system` row is a notice, such as a slash
    /// command's reply (headway:dave/repeat-garage-liberty): it is drawn as
    /// markdown, and a right-click on it offers Copy.
    #[test]
    fn system_row_drawn_in_agentic_mode_with_copy() {
        let mut harness = system_row_harness(CONTEXT_REPLY, true);
        // Drawn as markdown: the heading without its `##`, and each table cell
        // as its own label rather than one line of pipes.
        harness.get_by_label("155.7k");
        let node = harness.get_by_label("Context Usage");
        let bounds = node.accesskit_node().raw_bounds().expect("notice bounds");
        let pos = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();
        harness.run();
        assert!(harness.query_by_label("Copy").is_some());
    }

    /// The `system` notice rows an agentic session draws, as markdown: a
    /// `/context` reply's heading and table, and a `/model` confirmation. Render with
    /// `scripts/snapshot-test snapshot_system_notice_rows`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_system_notice_rows() {
        let context = SystemMessage::from(CONTEXT_REPLY);
        let model = SystemMessage::from("Model set to `opus` for the next turn");
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 200.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                DaveUi::system_message_ui(&context, true, ui);
                ui.add_space(4.0);
                DaveUi::system_message_ui(&model, true, ui);
            });
        harness.run();
        harness.snapshot("system_notice_rows");
    }

    /// `scripts/snapshot-test snapshot_executed_tool_results`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_executed_tool_results() {
        let results = executed_tool_fixtures();
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 120.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                    ui.add_space(4.0);
                }
            });

        harness.run();
        harness.snapshot("executed_tool_results");
    }

    /// Visualize the *hover affordance* on a collapsible row: with the pointer
    /// over the Bash-with-output row, the whole line gets a subtle rounded
    /// highlight behind it, signalling that clicking anywhere on the line (not
    /// just the chevron) toggles the body. Render with
    /// `scripts/snapshot-test snapshot_executed_tool_results_hover`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_executed_tool_results_hover() {
        let results = executed_tool_fixtures();
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 120.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                    ui.add_space(4.0);
                }
            });

        // Park the pointer over the first (collapsible) row so the hover
        // highlight paints. The first row sits a few px below the top margin.
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(egui::pos2(200.0, 14.0)));
        harness.run();
        harness.snapshot("executed_tool_results_hover");
    }

    /// Visualize the Bash-with-output row expanded, revealing its stdout block.
    /// Render with `scripts/snapshot-test snapshot_executed_tool_results_expanded`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_executed_tool_results_expanded() {
        let results = executed_tool_fixtures();
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 200.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                // Seed the ls row's disclosure open so the stdout block renders.
                // `executed_tool_ui` keys its expand flag off `exec_output` +
                // summary against this same `ui`'s id, so the key matches.
                let expand_id = ui.scope_id().with("exec_output").with("`ls -la crates`");
                ui.data_mut(|d| d.insert_temp(expand_id, true));
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                    ui.add_space(4.0);
                }
            });

        harness.run();
        harness.snapshot("executed_tool_results_expanded");
    }

    /// A cursor move pulls the cursor's block into view: with a transcript
    /// taller than its viewport, putting the cursor on the *last* block scrolls
    /// the area down to it. This is the only scroll plumbing in the chat — the
    /// live `ScrollArea` otherwise just sticks to the bottom.
    #[test]
    fn a_cursor_move_scrolls_its_block_into_view() {
        // Enough default-expanded Bash rows to overflow the viewport several
        // times over. Distinct summaries so each keys its own block.
        let results: Vec<_> = (0..20)
            .map(|i| crate::messages::ExecutedTool {
                tool_name: "Bash".to_string(),
                summary: format!("`echo {i}`"),
                output: Some(format!("{i}").into()),
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            })
            .collect();

        // The nav outlives the render closure so the test can move the cursor
        // between frames, exactly as a keybinding will.
        let nav = std::rc::Rc::new(std::cell::RefCell::new(BlockNav::default()));
        let offset = std::rc::Rc::new(std::cell::Cell::new(0.0));

        let render_nav = nav.clone();
        let render_offset = offset.clone();
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(420.0, 120.0))
            .build_ui(move |ui| {
                let mut nav = render_nav.borrow_mut();
                nav.begin_frame();
                let out = egui::ScrollArea::vertical().show(ui, |ui| {
                    for result in &results {
                        DaveUi::executed_tool_ui(result, &mut nav, ui);
                    }
                });
                render_offset.set(out.state.offset.y);
            });

        harness.run();
        assert_eq!(offset.get(), 0.0, "starts at the top of the transcript");

        nav.borrow_mut().last();
        harness.run();
        assert!(
            offset.get() > 0.0,
            "the cursor's block scrolled into view (offset {})",
            offset.get()
        );
    }

    /// Visualize the keyboard **block cursor** on each of the three collapsible
    /// row shapes at once — a tool-result header, a subagent's tool list and a
    /// responded permission — since no two of them paint a background the same
    /// way. The cursor washes the row in a tint of the selection hue, through
    /// the same reserved paint slot the tool header's hover highlight uses, and
    /// outranks that hover.
    ///
    /// Each section drives its own `BlockNav` with the cursor seeded at 0, which
    /// is what lets one image show all three lit; a real chat has one nav, and
    /// so one lit row. Render with
    /// `scripts/snapshot-test snapshot_block_cursor_rows`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_block_cursor_rows() {
        let results = executed_tool_fixtures();
        let subagent = crate::messages::SubagentInfo {
            task_id: "task-1".to_string(),
            description: "audit the login flow".to_string(),
            subagent_type: "Explore".to_string(),
            status: crate::messages::SubagentStatus::Completed,
            output: String::new(),
            max_output_size: 1000,
            tool_results: vec![crate::messages::ExecutedTool {
                tool_name: "Read".to_string(),
                summary: "lib.rs (128 lines)".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            }],
            background: false,
            agent_id: None,
            transcript: Default::default(),
        };
        let request = PermissionRequest::new(
            Uuid::new_v4(),
            "Bash".to_string(),
            json!({ "command": "cargo test --all -- --nocapture" }),
            None,
            Some(PermissionResponseType::Allowed),
            None,
        );

        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 200.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                // Fill the harness so the full-width row highlights sit on the
                // panel rather than running off it.
                ui.set_width(ui.available_width());

                // Collapse the Bash row so the cursor lands on a one-line row,
                // which is what a keyboard walk through a transcript mostly
                // sees. Same `exec_output` + summary key `executed_tool_ui` uses.
                let expand_id = ui.scope_id().with("exec_output").with("`ls -la crates`");
                ui.data_mut(|d| d.insert_temp(expand_id, false));

                let mut nav = BlockNav::default();
                nav.seed_cursor(0);
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                    ui.add_space(4.0);
                }

                ui.add_space(8.0);
                let mut nav = BlockNav::default();
                nav.seed_cursor(0);
                DaveUi::subagent_ui(&subagent, &mut nav, ui);

                ui.add_space(8.0);
                let mut nav = BlockNav::default();
                nav.seed_cursor(0);
                let mut input = String::new();
                let mut focus = false;
                let mut dave_ui = DaveUi::new(
                    false,
                    1,
                    &[],
                    &mut input,
                    &mut focus,
                    &mut nav,
                    AiMode::Agentic,
                );
                dave_ui.permission_request_ui(&request, None, ui);
            });

        harness.run();
        harness.snapshot("block_cursor_rows");
    }

    /// Visualize the call-summary rows for the newer SDK subagent/skill/messaging
    /// tools (headway:dave/make-stem-clerk, fancy-bulb-anger, fresh-pistol-depend):
    /// a Skill invocation, an Agent (subagent) launch mirroring Task, and a
    /// SendMessage agent-to-agent send. Each renders as a plain one-liner whose
    /// summary leads with its most identifying arg. These tools once hit the blank
    /// `_` arm and showed a bare greyed name; this guards that they now carry a
    /// summary. Render with `scripts/snapshot-test snapshot_agentic_tool_summaries`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_agentic_tool_summaries() {
        // Summaries mirror what `format_tool_summary` produces for each tool
        // (exercised directly in backend::tool_summary's unit tests); here we
        // render the resulting rows.
        let results = vec![
            crate::messages::ExecutedTool {
                tool_name: "Skill".to_string(),
                summary: "headway show dave/measure-crawl-goat".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
            crate::messages::ExecutedTool {
                tool_name: "Agent".to_string(),
                summary: "audit the login flow (Explore)".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
            crate::messages::ExecutedTool {
                tool_name: "SendMessage".to_string(),
                summary: "→ researcher: assign task 1".to_string(),
                output: None,
                parent_task_id: None,
                file_update: None,
                tool_use_id: None,
            },
        ];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 120.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                let mut nav = BlockNav::default();
                for result in &results {
                    DaveUi::executed_tool_ui(result, &mut nav, ui);
                    ui.add_space(4.0);
                }
            });

        harness.run();
        harness.snapshot("agentic_tool_summaries");
    }

    /// The rows jb55 saw glued in the live app: a `Skill` call and a
    /// background subagent, drawn in the `Ui` Dave gets inside the app
    /// ([`crate::ui::test_frame`]): the chrome's fonts, fill and zero gap, then
    /// Dave's own gap back.
    fn tool_rows_under_chrome_ui(ui: &mut egui::Ui) {
        crate::ui::test_frame(ui, tool_rows_ui);
    }

    fn tool_rows_ui(ui: &mut egui::Ui) {
        let skill = crate::messages::ExecutedTool {
            tool_name: "Skill".to_string(),
            summary: "code-review e8f3670c5e5b".to_string(),
            output: None,
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        };
        let subagent = SubagentInfo {
            task_id: "toolu_agent".to_string(),
            description: "code-review".to_string(),
            subagent_type: "agent".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4096,
            tool_results: Vec::new(),
            background: true,
            agent_id: None,
            transcript: Default::default(),
        };
        let mut nav = BlockNav::default();
        DaveUi::executed_tool_ui(&skill, &mut nav, ui);
        ui.add_space(4.0);
        DaveUi::subagent_ui(&subagent, &mut nav, ui);
    }

    /// Under the chrome's zero gap, Dave's own root spacing keeps a tool row's
    /// name off its summary and a subagent's description off its "in
    /// background" flag (headway:headway/skill-either-bench).
    #[test]
    fn tool_row_labels_keep_a_gap_under_the_chrome() {
        let mut harness = notedeck::test_harness::app_builder()
            .with_size(egui::Vec2::new(460.0, 120.0))
            .build_ui(tool_rows_under_chrome_ui);
        harness.run();

        let gap = |left: &str, right: &str| {
            let x0 = harness
                .get_by_label(right)
                .accesskit_node()
                .bounding_box()
                .unwrap()
                .x0;
            let x1 = harness
                .get_by_label(left)
                .accesskit_node()
                .bounding_box()
                .unwrap()
                .x1;
            (x0 - x1) as f32
        };
        let min = notedeck::tokens::SPACING_SM - 0.5;
        assert!(
            gap("Skill", "code-review e8f3670c5e5b") >= min,
            "the tool name must not run into its summary"
        );
        assert!(
            gap("code-review", "· in background") >= min,
            "a subagent's description must not run into its background flag"
        );
    }

    /// The glued rows from the live app, in the app's fonts, fill and spacing.
    /// Render with
    /// `scripts/snapshot-test snapshot_tool_rows_under_chrome_spacing`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_tool_rows_under_chrome_spacing() {
        let mut harness = notedeck::test_harness::app_builder()
            .with_size(egui::Vec2::new(460.0, 70.0))
            .renderer(notedeck::software_renderer())
            .build_ui(tool_rows_under_chrome_ui);
        harness.run();
        harness.snapshot("tool_rows_under_chrome_spacing");
    }

    /// A `WebSearch` result (headway:dave/smart-wreck-weapon): the header row
    /// leads with the quoted query and the expanded body shows the returned
    /// results list. WebSearch once hit the blank `_` summary arm and rendered a
    /// bare greyed name with no results; this guards the query-in-header +
    /// results-in-body layout. Unlike Bash, the summary (the query) is *not*
    /// echoed in the body — only the results are.
    /// Render with `scripts/snapshot-test snapshot_websearch_result`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_websearch_result() {
        let summary = "'rust async tokio cancellation'";
        let output = "1. Tokio — Graceful Shutdown (tokio.rs)\n2. CancellationToken docs (docs.rs)\n3. \"How to cancel a future\" (users.rust-lang.org)";
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "WebSearch".to_string(),
            summary: summary.to_string(),
            output: Some(output.into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 140.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                ui.vertical(|ui| {
                    let mut nav = BlockNav::default();
                    for result in &results {
                        DaveUi::executed_tool_ui(result, &mut nav, ui);
                        ui.add_space(4.0);
                    }
                });
            });

        harness.run();
        harness.snapshot("websearch_result");
    }

    /// Regression guard for the expanded unified body (headway:dave/sting-february-sausage):
    /// a Bash result with a long command *and* output with an unbreakable token,
    /// rendered beside a wide sibling that stretches the column past the
    /// viewport. Expanded, the body shows the full command, a divider, then the
    /// output — both wrapping within the visible width (break-anywhere for the
    /// unbreakable token) instead of running off the right edge.
    /// Render with `scripts/snapshot-test snapshot_tool_output_wraps_in_viewport`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_tool_output_wraps_in_viewport() {
        let summary = "`headway --board commerce desc headway:commerce/gallery-mansion-suggest \"Drop the notification claim table; best-effort send + operator-review transition guard replaces it, no reaper, no queue\"`";
        let output = "ok (1 events)\n/Users/jb55/dev/hydra/commerce/apps/api/src/onboarding/onboarding.service.ts:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nexit 0";
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: summary.to_string(),
            output: Some(output.into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(700.0, 260.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            let expand_id = ui.scope_id().with("exec_output").with(summary);
                            ui.data_mut(|d| d.insert_temp(expand_id, true));
                            let mut nav = BlockNav::default();
                            for result in &results {
                                DaveUi::executed_tool_ui(result, &mut nav, ui);
                                ui.add_space(4.0);
                            }
                        });
                    });
            });

        harness.run();
        harness.snapshot("tool_output_wraps_in_viewport");
    }

    /// Regression guard for the *collapsed* header (headway:dave/sting-february-sausage):
    /// a Bash result whose summary is a very long command, at a realistic chat
    /// width. The header must stay one line, truncating the command with an
    /// ellipsis, instead of overrunning the right edge — the overflow jb55 hit
    /// (https://jb55.com/s/wrap-broke.png). Expanding reveals the full command
    /// (see `snapshot_tool_output_wraps_in_viewport`).
    /// Render with `scripts/snapshot-test snapshot_collapsed_bash_header_truncates`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_collapsed_bash_header_truncates() {
        let summary = "`headway --board commerce desc headway:commerce/gallery-mansion-suggest \"Drop the notification claim table; best-effort send + operator-review transition guard replaces it, no reaper, no queue\"`";
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: summary.to_string(),
            output: Some("ok (1 events)\nexit 0".into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(460.0, 80.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                ui.vertical(|ui| {
                    // Bash-output rows start expanded; force the collapsed state
                    // to exercise the header-truncation path this test guards.
                    let expand_id = ui.scope_id().with("exec_output").with(summary);
                    ui.data_mut(|d| d.insert_temp(expand_id, false));
                    let mut nav = BlockNav::default();
                    for result in &results {
                        DaveUi::executed_tool_ui(result, &mut nav, ui);
                        ui.add_space(4.0);
                    }
                });
            });

        harness.run();
        harness.snapshot("collapsed_bash_header_truncates");
    }

    /// `n` numbered lines, `line 1` to `line n`, as a command's stdout.
    fn numbered_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    /// A one-result harness drawing a Bash row with `summary` and `output`,
    /// inside a scroll area like the chat's.
    fn bash_output_harness(
        summary: &str,
        output: String,
        size: egui::Vec2,
    ) -> egui_kittest::Harness<'static> {
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: summary.to_string(),
            output: Some(output.into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        notedeck::test_harness::lenient_builder()
            .with_size(size)
            .build_ui(move |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let mut nav = BlockNav::default();
                        for result in &results {
                            DaveUi::executed_tool_ui(result, &mut nav, ui);
                        }
                    });
            })
    }

    /// A long Bash output opens expanded but clamped (headway:dave/beef-calm-cross):
    /// its first and last four lines with a gap counting what lies between, a
    /// "show all" footer, and the line count in the header. Nothing from the
    /// middle is drawn.
    #[test]
    fn long_bash_output_is_clamped_to_head_and_tail() {
        let mut harness = bash_output_harness(
            "`sed -n 1,523p big.rs`",
            numbered_lines(523),
            egui::Vec2::new(480.0, 400.0),
        );
        harness.run();

        harness.get_by_label("· 523 lines");
        harness.get_by_label("line 1\nline 2\nline 3\nline 4");
        harness.get_by_label("┄┄ 515 lines hidden ┄┄");
        harness.get_by_label("line 520\nline 521\nline 522\nline 523");
        harness.get_by_label("▸ show all");
        assert!(harness.query_by_label_contains("line 200").is_none());
    }

    /// Output that fits the clamp is drawn whole, with no gap and no footer.
    #[test]
    fn short_bash_output_is_shown_whole() {
        let mut harness =
            bash_output_harness("`ls`", numbered_lines(9), egui::Vec2::new(480.0, 400.0));
        harness.run();

        harness.get_by_label("· 9 lines");
        harness.get_by_label(numbered_lines(9).trim_end());
        assert!(harness.query_by_label_contains("lines hidden").is_none());
        assert!(harness.query_by_label("▸ show all").is_none());
    }

    /// "Show all" swaps the preview for the whole output in a bounded scroll
    /// area, which lays out only the lines in sight; "show less" swaps back.
    #[test]
    fn show_all_toggles_full_output() {
        let mut harness = bash_output_harness(
            "`sed -n 1,523p big.rs`",
            numbered_lines(523),
            egui::Vec2::new(480.0, 400.0),
        );
        harness.run();

        harness.get_by_label("▸ show all").click();
        harness.run();

        // Each line is its own row now, and the gap is gone.
        harness.get_by_label("line 5");
        assert!(harness.query_by_label_contains("lines hidden").is_none());
        // Bounded and virtualized: the far end isn't laid out until scrolled to.
        assert!(harness.query_by_label("line 523").is_none());

        harness.get_by_label("▾ show less").click();
        harness.run();
        harness.get_by_label("┄┄ 515 lines hidden ┄┄");
        harness.get_by_label("▸ show all");
    }

    /// A command echo past three wrapped rows is clipped too, so even a short
    /// output gets a "show all" footer, and opening it keeps the whole command.
    #[test]
    fn long_command_echo_is_clamped() {
        let command: String = (0..60).map(|i| format!("arg{i} ")).collect();
        let command = command.trim_end();
        let mut harness = bash_output_harness(
            &format!("`echo {command}`"),
            "ok".to_string(),
            egui::Vec2::new(360.0, 400.0),
        );
        harness.run();

        // The output fits, so only the clipped command can raise the footer.
        // (Its label can't show the clip: an elided galley still reports its
        // whole text to accesskit.)
        harness.get_by_label("▸ show all").click();
        harness.run();
        harness.get_by_label(&format!("echo {command}"));
        harness.get_by_label("▾ show less");
    }

    /// The clamped preview of a long `sed` — the case that used to fill the
    /// chat — and the same row with "show all" open.
    /// Render with `scripts/snapshot-test snapshot_clamped_bash_output`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_clamped_bash_output() {
        let summary = "`sed -n 1,120p crates/notedeck_dave/src/ui/dave.rs`";
        let output: String = (1..=120)
            .map(|i| format!("{i:>4}  fn line_{i}() -> usize {{ {i} }}\n"))
            .collect();
        let results = vec![crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: summary.to_string(),
            output: Some(output.into()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }];
        let mut harness = notedeck::test_harness::lenient_builder()
            .with_size(egui::Vec2::new(520.0, 360.0))
            .renderer(notedeck::software_renderer())
            .build_ui(move |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let mut nav = BlockNav::default();
                        for result in &results {
                            DaveUi::executed_tool_ui(result, &mut nav, ui);
                        }
                    });
            });
        harness.run();
        harness.snapshot("clamped_bash_output");

        harness.get_by_label("▸ show all").click();
        harness.run();
        harness.snapshot("clamped_bash_output_show_all");
    }

    /// The chat input in each mode, framed the way `DaveUi` frames it, under
    /// notedeck's own dark and light visuals rather than egui's defaults: the
    /// cue is a colour change, so the default theme would show the wrong one.
    /// Rows: insert, normal while idle, normal while a turn runs, and normal
    /// with a draft typed (the cue is hidden, so the dimmed text carries it).
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_input_normal_mode_cue() {
        use super::{input_fill, InputMode, InputboxLayout};
        use notedeck::Localization;

        let rows = [
            ("", InputMode::Insert),
            ("", InputMode::Normal { can_stop: false }),
            ("", InputMode::Normal { can_stop: true }),
            ("a draft", InputMode::Normal { can_stop: false }),
        ];
        let themes = [
            notedeck::theme::dark_mode(false),
            notedeck::theme::light_mode(),
        ];

        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(1000.0, 370.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(
                move |ui, i18n: &mut Localization| {
                    ui.columns(themes.len(), |columns| {
                        for (ui, visuals) in columns.iter_mut().zip(&themes) {
                            ui.style_mut().visuals = visuals.clone();
                            ui.painter()
                                .rect_filled(ui.max_rect(), 0.0, visuals.panel_fill);
                            for (text, mode) in rows {
                                let mut text = text.to_string();
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::same(
                                        notedeck::tokens::SPACING_SM as i8,
                                    ))
                                    .fill(input_fill(ui.visuals(), mode != InputMode::Insert))
                                    .corner_radius(notedeck::tokens::RADIUS_LG)
                                    .show(ui, |ui| {
                                        InputboxLayout::new(&mut text, i18n, mode)
                                            .show_stop(mode == InputMode::Normal { can_stop: true })
                                            .show(ui);
                                    });
                                ui.add_space(notedeck::tokens::SPACING_SM);
                            }
                        }
                    });
                },
                Localization::default(),
            );
        harness.run();
        harness.snapshot("dave_input_normal_mode_cue");
    }
}
