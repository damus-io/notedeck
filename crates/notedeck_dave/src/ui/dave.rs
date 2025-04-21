use crate::{
    messages::Message,
    tools::{PresentNotesCall, QueryCall, QueryContext, ToolCall, ToolCalls, ToolResponse},
};
use egui::{Align, Key, KeyboardShortcut, Layout, Modifiers};
use nostrdb::Transaction;
use notedeck::{AppContext, NoteContext};
use notedeck_ui::{icons::search_icon, NoteOptions};

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
    /// Generate a send response to the controller
    fn send() -> Self {
        DaveResponse {
            action: Some(DaveAction::Send),
        }
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
        // Scroll area for chat messages
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
                        //.stroke(stroke)
                        .corner_radius(12.0)
                        .show(ui, |ui| self.inputbox(ui))
                        .inner;

                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            Self::chat_frame(ui.ctx()).show(ui, |ui| {
                                ui.vertical(|ui| {
                                    self.render_chat(app_ctx, ui);
                                });
                            });
                        });

                    r
                })
                .inner
            })
            .inner
    }

    /// Render a chat message (user, assistant, tool call/response, etc)
    fn render_chat(&self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        for message in self.chat {
            match message {
                Message::User(msg) => self.user_chat(msg, ui),
                Message::Assistant(msg) => self.assistant_chat(msg, ui),
                Message::ToolResponse(msg) => Self::tool_response_ui(msg, ui),
                Message::System(_msg) => {
                    // system prompt is not rendered. Maybe we could
                    // have a debug option to show this
                }
                Message::ToolCalls(toolcalls) => {
                    Self::tool_calls_ui(ctx, toolcalls, ui);
                }
            }
        }
    }

    fn tool_response_ui(_tool_response: &ToolResponse, _ui: &mut egui::Ui) {
        //ui.label(format!("tool_response: {:?}", tool_response));
    }

    fn search_call_ui(query_call: &QueryCall, ui: &mut egui::Ui) {
        ui.add(search_icon(16.0, 16.0));
        ui.add_space(8.0);
        let context = match query_call.context() {
            QueryContext::Profile => "profile ",
            QueryContext::Any => "",
            QueryContext::Home => "home ",
        };

        //TODO: fix this to support any query
        if let Some(search) = query_call.search() {
            ui.label(format!("Querying {context}for '{search}'"));
        } else {
            ui.label(format!("Querying {:?}", &query_call));
        }
    }

    /// The ai has asked us to render some notes, so we do that here
    fn present_notes_ui(ctx: &mut AppContext, call: &PresentNotesCall, ui: &mut egui::Ui) {
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

                    for note_id in &call.note_ids {
                        let Ok(note) = note_context.ndb.get_note_by_id(&txn, note_id.bytes())
                        else {
                            continue;
                        };

                        let mut note_view = notedeck_ui::NoteView::new(
                            &mut note_context,
                            &None,
                            &note,
                            NoteOptions::default(),
                        )
                        .preview_style();

                        // TODO: remove current account thing, just add to note context
                        ui.add_sized([400.0, 400.0], &mut note_view);
                    }
                });
            });
    }

    fn tool_calls_ui(ctx: &mut AppContext, toolcalls: &[ToolCall], ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            for call in toolcalls {
                match call.calls() {
                    ToolCalls::PresentNotes(call) => Self::present_notes_ui(ctx, call, ui),
                    ToolCalls::Query(search_call) => {
                        ui.horizontal(|ui| {
                            egui::Frame::new()
                                .inner_margin(10.0)
                                .corner_radius(10.0)
                                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                                .show(ui, |ui| {
                                    Self::search_call_ui(search_call, ui);
                                })
                        });
                    }
                }
            }
        });
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
