use crate::{
    messages::Message,
    tools::{PresentNotesCall, QueryCall, ToolCall, ToolCalls, ToolResponse},
};
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::{Ndb, Transaction};
use notedeck::{AppContext, NoteAction, NoteContext};
use notedeck_ui::{icons::search_icon, NoteOptions, ProfilePic};

/// DaveUi holds all of the data it needs to render itself
pub struct DaveUi<'a> {
    chat: &'a [Message],
    input: &'a mut String,
}

/// The response the app generates. The response contains an optional
/// action to take.
#[derive(Default, Clone, Debug)]
pub struct DaveResponse {
    pub action: Option<DaveAction>,
}

impl DaveResponse {
    fn new(action: DaveAction) -> Self {
        DaveResponse {
            action: Some(action),
        }
    }

    fn note(action: NoteAction) -> DaveResponse {
        Self::new(DaveAction::Note(action))
    }

    fn or(self, r: DaveResponse) -> DaveResponse {
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
#[derive(Clone, Debug)]
pub enum DaveAction {
    /// The action generated when the user sends a message to dave
    Send,
    NewChat,
    Note(NoteAction),
}

impl<'a> DaveUi<'a> {
    pub fn new(chat: &'a [Message], input: &'a mut String) -> Self {
        DaveUi { chat, input }
    }

    fn chat_margin(ctx: &egui::Context) -> i8 {
        if notedeck::ui::is_narrow(ctx) {
            20
        } else {
            100
        }
    }

    fn chat_frame(ctx: &egui::Context) -> egui::Frame {
        let margin = Self::chat_margin(ctx);
        egui::Frame::new().inner_margin(egui::Margin {
            left: margin,
            right: margin,
            top: 50,
            bottom: 0,
        })
    }

    /// The main render function. Call this to render Dave
    pub fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let mut action: Option<DaveAction> = None;
        // Scroll area for chat messages
        let new_resp = {
            let mut rect = ui.available_rect_before_wrap();
            rect = rect.translate(egui::vec2(20.0, 20.0));
            rect.set_width(32.0);
            rect.set_height(32.0);
            ui.put(rect, new_chat_button())
        };

        if new_resp.clicked() {
            action = Some(DaveAction::NewChat);
        } else if new_resp.hovered() {
            notedeck_ui::show_pointer(ui);
        }

        egui::Frame::NONE
            .show(ui, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let margin = Self::chat_margin(ui.ctx());

                    let r = egui::Frame::new()
                        .outer_margin(egui::Margin {
                            left: margin,
                            right: margin,
                            top: 0,
                            bottom: 100,
                        })
                        .inner_margin(egui::Margin::same(8))
                        .fill(ui.visuals().extreme_bg_color)
                        .corner_radius(12.0)
                        .show(ui, |ui| self.inputbox(ui))
                        .inner;

                    let note_action = egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            Self::chat_frame(ui.ctx())
                                .show(ui, |ui| {
                                    ui.vertical(|ui| self.render_chat(app_ctx, ui)).inner
                                })
                                .inner
                        })
                        .inner;

                    if let Some(action) = note_action {
                        DaveResponse::note(action)
                    } else {
                        r
                    }
                })
                .inner
            })
            .inner
            .or(DaveResponse { action })
    }

    /// Render a chat message (user, assistant, tool call/response, etc)
    fn render_chat(&self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<NoteAction> {
        let mut action: Option<NoteAction> = None;
        for message in self.chat {
            let r = match message {
                Message::User(msg) => {
                    self.user_chat(msg, ui);
                    None
                }
                Message::Assistant(msg) => {
                    self.assistant_chat(msg, ui);
                    None
                }
                Message::ToolResponse(msg) => {
                    Self::tool_response_ui(msg, ui);
                    None
                }
                Message::System(_msg) => {
                    // system prompt is not rendered. Maybe we could
                    // have a debug option to show this
                    None
                }
                Message::ToolCalls(toolcalls) => Self::tool_calls_ui(ctx, toolcalls, ui),
            };

            if r.is_some() {
                action = r;
            }
        }

        action
    }

    fn tool_response_ui(_tool_response: &ToolResponse, _ui: &mut egui::Ui) {
        //ui.label(format!("tool_response: {:?}", tool_response));
    }

    fn search_call_ui(ctx: &mut AppContext, query_call: &QueryCall, ui: &mut egui::Ui) {
        ui.add(search_icon(16.0, 16.0));
        ui.add_space(8.0);

        query_call_ui(ctx.img_cache, ctx.ndb, query_call, ui);
    }

    /// The ai has asked us to render some notes, so we do that here
    fn present_notes_ui(
        ctx: &mut AppContext,
        call: &PresentNotesCall,
        ui: &mut egui::Ui,
    ) -> Option<NoteAction> {
        let mut note_context = NoteContext {
            ndb: ctx.ndb,
            img_cache: ctx.img_cache,
            note_cache: ctx.note_cache,
            zaps: ctx.zaps,
            pool: ctx.pool,
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
                                        &None,
                                        &note,
                                        NoteOptions::default(),
                                    )
                                    .preview_style()
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
                        ui.label(format!("invalid tool call: {:?}", err));
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

    fn inputbox(&mut self, ui: &mut egui::Ui) -> DaveResponse {
        //ui.add_space(Self::chat_margin(ui.ctx()) as f32);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
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
                        .hint_text(egui::RichText::new("Ask dave anything...").weak())
                        .frame(false),
                );

                if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    DaveResponse::send()
                } else {
                    DaveResponse::none()
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
                    ui.label(msg);
                })
        });
    }

    fn assistant_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(msg).wrap_mode(egui::TextWrapMode::Wrap));
        });
    }
}

fn new_chat_button() -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        let img_size = 24.0;
        let max_size = 32.0;

        let img_data = egui::include_image!("../../../../assets/icons/newmessage_64.png");
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper = notedeck_ui::anim::AnimationHelper::new(
            ui,
            "new-chat-button",
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

fn query_call_ui(cache: &mut notedeck::Images, ndb: &Ndb, query: &QueryCall, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.x = 8.0;
    if let Some(pubkey) = query.author() {
        let txn = Transaction::new(ndb).unwrap();
        pill_label_ui(
            "author",
            move |ui| {
                ui.add(
                    ProfilePic::from_profile_or_default(
                        cache,
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
