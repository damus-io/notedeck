use crate::{
    config::DaveSettings,
    messages::{
        Message, PermissionRequest, PermissionResponse, PermissionResponseType, ToolResult,
    },
    tools::{PresentNotesCall, QueryCall, ToolCall, ToolCalls, ToolResponse},
};
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::{Ndb, Transaction};
use notedeck::{
    tr, Accounts, AppContext, Images, Localization, MediaJobSender, NoteAction, NoteContext,
};
use notedeck_ui::{app_images, icons::search_icon, NoteOptions, ProfilePic};
use uuid::Uuid;

/// DaveUi holds all of the data it needs to render itself
pub struct DaveUi<'a> {
    chat: &'a [Message],
    trial: bool,
    input: &'a mut String,
    compact: bool,
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
}

impl<'a> DaveUi<'a> {
    pub fn new(trial: bool, chat: &'a [Message], input: &'a mut String) -> Self {
        DaveUi {
            trial,
            chat,
            input,
            compact: false,
        }
    }

    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
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

                    // Reduce bottom margin in compact mode to prevent overflow
                    let bottom_margin = if self.compact { 20 } else { 100 };

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

                    let chat_response = egui::ScrollArea::vertical()
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
    fn render_chat(&self, ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let mut response = DaveResponse::default();
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
                    if let Some(action) = Self::permission_request_ui(request, ui) {
                        response = DaveResponse::new(action);
                    }
                }
                Message::ToolResult(result) => {
                    Self::tool_result_ui(result, ui);
                }
            };
        }

        response
    }

    fn tool_response_ui(_tool_response: &ToolResponse, _ui: &mut egui::Ui) {
        //ui.label(format!("tool_response: {:?}", tool_response));
    }

    /// Render a permission request with Allow/Deny buttons or response state
    fn permission_request_ui(request: &PermissionRequest, ui: &mut egui::Ui) -> Option<DaveAction> {
        let mut action = None;

        let inner_margin = 8.0;
        let corner_radius = 6.0;
        let spacing_x = 8.0;

        ui.spacing_mut().item_spacing.x = spacing_x;

        match request.response {
            Some(PermissionResponseType::Allowed) => {
                // Responded state: Allowed
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
                // Parse tool input for display
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

                                Self::permission_buttons(request, ui, &mut action);
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

                                Self::permission_buttons(request, ui, &mut action);
                            });
                        } else {
                            // Fallback: show JSON
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&request.tool_name).strong());

                                Self::permission_buttons(request, ui, &mut action);
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

        action
    }

    /// Render Allow/Deny buttons aligned to the right
    fn permission_buttons(
        request: &PermissionRequest,
        ui: &mut egui::Ui,
        action: &mut Option<DaveAction>,
    ) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Deny button (red)
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Deny")
                            .color(ui.visuals().widgets.active.fg_stroke.color),
                    )
                    .fill(egui::Color32::from_rgb(178, 34, 34)),
                )
                .clicked()
            {
                *action = Some(DaveAction::PermissionResponse {
                    request_id: request.id,
                    response: PermissionResponse::Deny {
                        reason: "User denied".into(),
                    },
                });
            }

            // Allow button (green)
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Allow")
                            .color(ui.visuals().widgets.active.fg_stroke.color),
                    )
                    .fill(egui::Color32::from_rgb(34, 139, 34)),
                )
                .clicked()
            {
                *action = Some(DaveAction::PermissionResponse {
                    request_id: request.id,
                    response: PermissionResponse::Allow,
                });
            }
        });
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

    fn search_call_ui(ctx: &mut AppContext, query_call: &QueryCall, ui: &mut egui::Ui) {
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
                if ui
                    .add(egui::Button::new(tr!(
                        i18n,
                        "Ask",
                        "Button to send message to Dave AI assistant"
                    )))
                    .clicked()
                {
                    dave_response = DaveResponse::send();
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
                    ui.add(egui::Label::new(msg).selectable(true));
                })
        });
    }

    fn assistant_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(msg)
                    .wrap_mode(egui::TextWrapMode::Wrap)
                    .selectable(true),
            );
        });
    }
}

fn settings_button(dark_mode: bool) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = 32.0;

        let img = if dark_mode {
            app_images::settings_dark_image()
        } else {
            app_images::settings_light_image()
        }
        .max_width(img_size);

        let helper = notedeck_ui::anim::AnimationHelper::new(
            ui,
            "settings-button",
            egui::vec2(max_size, max_size),
        );

        let cur_img_size = helper.scale_1d_pos(img_size);
        img.paint_at(
            ui,
            helper
                .get_animation_rect()
                .shrink((max_size - cur_img_size) / 2.0),
        );

        helper.take_animation_response()
    }
}

fn query_call_ui(
    cache: &mut notedeck::Images,
    ndb: &Ndb,
    query: &QueryCall,
    jobs: &MediaJobSender,
    ui: &mut egui::Ui,
) {
    ui.spacing_mut().item_spacing.x = 8.0;
    if let Some(pubkey) = query.author() {
        let txn = Transaction::new(ndb).unwrap();
        pill_label_ui(
            "author",
            move |ui| {
                ui.add(
                    &mut ProfilePic::from_profile_or_default(
                        cache,
                        jobs,
                        ndb.get_profile_by_pubkey(&txn, pubkey.bytes())
                            .ok()
                            .as_ref(),
                    )
                    .size(ProfilePic::small_size() as f32),
                );
            },
            ui,
        );
    }

    if let Some(limit) = query.limit {
        pill_label("limit", &limit.to_string(), ui);
    }

    if let Some(since) = query.since {
        pill_label("since", &since.to_string(), ui);
    }

    if let Some(kind) = query.kind {
        pill_label("kind", &kind.to_string(), ui);
    }

    if let Some(until) = query.until {
        pill_label("until", &until.to_string(), ui);
    }

    if let Some(search) = query.search.as_ref() {
        pill_label("search", search, ui);
    }
}

fn pill_label(name: &str, value: &str, ui: &mut egui::Ui) {
    pill_label_ui(
        name,
        move |ui| {
            ui.label(value);
        },
        ui,
    );
}

fn pill_label_ui(name: &str, mut value: impl FnMut(&mut egui::Ui), ui: &mut egui::Ui) {
    egui::Frame::new()
        .fill(ui.visuals().noninteractive().bg_fill)
        .inner_margin(egui::Margin::same(4))
        .corner_radius(egui::CornerRadius::same(10))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(ui.visuals().noninteractive().weak_bg_fill)
                .inner_margin(egui::Margin::same(4))
                .corner_radius(egui::CornerRadius::same(10))
                .stroke(egui::Stroke::new(
                    1.0,
                    ui.visuals().noninteractive().bg_stroke.color,
                ))
                .show(ui, |ui| {
                    ui.label(name);
                });

            value(ui);
        });
}

fn top_buttons_ui(app_ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<DaveAction> {
    // Scroll area for chat messages
    let mut action: Option<DaveAction> = None;
    let mut rect = ui.available_rect_before_wrap();
    rect = rect.translate(egui::vec2(20.0, 20.0));
    rect.set_height(32.0);
    rect.set_width(32.0);

    // Show session list button on mobile/narrow screens
    if notedeck::ui::is_narrow(ui.ctx()) {
        let r = ui
            .put(rect, egui::Button::new("\u{2630}").frame(false))
            .on_hover_text("Show chats")
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        if r.clicked() {
            action = Some(DaveAction::ShowSessionList);
        }

        rect = rect.translate(egui::vec2(30.0, 0.0));
    }

    let txn = Transaction::new(app_ctx.ndb).unwrap();
    let r = ui
        .put(
            rect,
            &mut pfp_button(
                &txn,
                app_ctx.accounts,
                app_ctx.img_cache,
                app_ctx.ndb,
                app_ctx.media_jobs.sender(),
            ),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if r.clicked() {
        action = Some(DaveAction::ToggleChrome);
    }

    // Settings button
    rect = rect.translate(egui::vec2(30.0, 0.0));
    let dark_mode = ui.visuals().dark_mode;
    let r = ui
        .put(rect, settings_button(dark_mode))
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    if r.clicked() {
        action = Some(DaveAction::OpenSettings);
    }

    action
}

fn pfp_button<'me, 'a>(
    txn: &'a Transaction,
    accounts: &Accounts,
    img_cache: &'me mut Images,
    ndb: &Ndb,
    jobs: &'me MediaJobSender,
) -> ProfilePic<'me, 'a> {
    let account = accounts.get_selected_account();
    let profile = ndb
        .get_profile_by_pubkey(txn, account.key.pubkey.bytes())
        .ok();

    ProfilePic::from_profile_or_default(img_cache, jobs, profile.as_ref())
        .size(24.0)
        .sense(egui::Sense::click())
}
