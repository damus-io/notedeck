use chrono::{DateTime, Duration, Local, NaiveDate};
use egui::{
    vec2, Align, Color32, CornerRadius, Frame, Key, Layout, Margin, RichText, ScrollArea, TextEdit,
};
use egui_extras::{Size, StripBuilder};
use enostr::Pubkey;
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::{
    name::get_display_name, tr, ui::is_narrow, Images, Localization, MediaJobSender, NostrName,
};
use notedeck_ui::{include_input, ProfilePic};

use crate::{
    cache::{
        Conversation, ConversationCache, ConversationId, ConversationState, ConversationStates,
    },
    convo_renderable::{ConversationItem, MessageType},
    nav::MessagesAction,
    nip17::{parse_chat_message, Nip17ChatMessage},
    ui::{local_datetime_from_nostr, title_label},
};

pub struct ConversationUi<'a> {
    conversation: &'a Conversation,
    state: &'a mut ConversationState,
    ndb: &'a Ndb,
    jobs: &'a MediaJobSender,
    img_cache: &'a mut Images,
    i18n: &'a mut Localization,
}

impl<'a> ConversationUi<'a> {
    pub fn new(
        conversation: &'a Conversation,
        state: &'a mut ConversationState,
        ndb: &'a Ndb,
        jobs: &'a MediaJobSender,
        img_cache: &'a mut Images,
        i18n: &'a mut Localization,
    ) -> Self {
        Self {
            conversation,
            state,
            ndb,
            jobs,
            img_cache,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, selected_pubkey: &Pubkey) -> Option<MessagesAction> {
        let txn = Transaction::new(self.ndb).expect("txn");

        let mut action = None;
        Frame::new().fill(ui.visuals().panel_fill).show(ui, |ui| {
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                ui.allocate_ui(vec2(ui.available_width(), 64.0), |ui| {
                    let comp_resp =
                        conversation_composer(ui, self.state, self.conversation.id, self.i18n);
                    if action.is_none() {
                        action = comp_resp.action;
                    }
                    comp_resp.composer_has_focus
                });
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .id_salt(ui.id().with(self.conversation.id))
                        .show(ui, |ui| {
                            conversation_history(
                                ui,
                                self.conversation,
                                self.state,
                                self.jobs,
                                self.ndb,
                                &txn,
                                self.img_cache,
                                selected_pubkey,
                                self.i18n,
                            );
                        });
                });
            })
        });

        action
    }
}

#[allow(clippy::too_many_arguments)]
fn conversation_history(
    ui: &mut egui::Ui,
    conversation: &Conversation,
    state: &mut ConversationState,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    txn: &Transaction,
    img_cache: &mut Images,
    selected_pk: &Pubkey,
    i18n: &mut Localization,
) {
    let renderable = &conversation.renderable;

    state.last_read = conversation
        .messages
        .messages_ordered
        .first()
        .map(|n| &n.note_ref)
        .copied();
    Frame::new()
        .inner_margin(Margin::symmetric(16, 0))
        .show(ui, |ui| {
            let today = Local::now().date_naive();
            let total = renderable.len();
            state.list.ui_custom_layout(ui, total, |ui, index| {
                let Some(renderable) = renderable.get(index) else {
                    return 1;
                };

                match renderable {
                    ConversationItem::Date(date) => render_date_line(ui, *date, &today, i18n),
                    ConversationItem::Message { msg_type, key } => {
                        render_chat_msg(
                            ui,
                            img_cache,
                            jobs,
                            ndb,
                            txn,
                            *key,
                            *msg_type,
                            selected_pk,
                        );
                    }
                };

                1
            });
        });
}

fn render_date_line(
    ui: &mut egui::Ui,
    date: NaiveDate,
    today: &NaiveDate,
    i18n: &mut Localization,
) {
    let label = format_day_heading(date, today, i18n);
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(label)
                    .strong()
                    .color(ui.visuals().weak_text_color()),
            )
            .wrap(),
        );
    });
    ui.add_space(4.0);
}

#[allow(clippy::too_many_arguments)]
fn render_chat_msg(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    txn: &Transaction,
    key: NoteKey,
    msg_type: MessageType,
    selected_pk: &Pubkey,
) {
    let Ok(note) = ndb.get_note_by_key(txn, key) else {
        tracing::error!("Could not get key {:?}", key);
        return;
    };

    let Some(chat_msg) = parse_chat_message(&note) else {
        tracing::error!("Could not parse chat message for note {key:?}");
        return;
    };

    match msg_type {
        MessageType::Standalone => {
            ui.add_space(2.0);
            render_msg_with_pfp(
                ui,
                img_cache,
                jobs,
                ndb,
                txn,
                selected_pk,
                msg_type,
                chat_msg,
            );
            ui.add_space(2.0);
        }
        MessageType::FirstInSeries => {
            ui.add_space(2.0);
            render_msg_no_pfp(ui, ndb, txn, selected_pk, msg_type, chat_msg);
        }
        MessageType::MiddleInSeries => {
            render_msg_no_pfp(ui, ndb, txn, selected_pk, msg_type, chat_msg);
        }
        MessageType::LastInSeries => {
            render_msg_with_pfp(
                ui,
                img_cache,
                jobs,
                ndb,
                txn,
                selected_pk,
                msg_type,
                chat_msg,
            );
            ui.add_space(2.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_msg_with_pfp(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    txn: &Transaction,
    selected_pk: &Pubkey,
    msg_type: MessageType,
    chat_msg: Nip17ChatMessage,
) {
    if selected_pk.bytes() == chat_msg.sender {
        self_chat_bubble(ui, chat_msg.message, msg_type, chat_msg.created_at);
        return;
    }

    let avatar_size = ProfilePic::medium_size() as f32;
    let profile = ndb.get_profile_by_pubkey(txn, chat_msg.sender).ok();
    let mut pic =
        ProfilePic::from_profile_or_default(img_cache, jobs, profile.as_ref()).size(avatar_size);
    ui.horizontal(|ui| {
        ui.add(&mut pic);
        ui.add_space(8.0);

        other_chat_bubble(ui, chat_msg, get_display_name(profile.as_ref()), msg_type);
    });
}

fn render_msg_no_pfp(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    txn: &Transaction,
    selected_pk: &Pubkey,
    msg_type: MessageType,
    chat_msg: Nip17ChatMessage,
) {
    if selected_pk.bytes() == chat_msg.sender {
        self_chat_bubble(ui, chat_msg.message, msg_type, chat_msg.created_at);
        return;
    }

    ui.horizontal(|ui| {
        ui.add_space(ProfilePic::medium_size() as f32 + ui.spacing().item_spacing.x + 8.0);
        let profile = ndb.get_profile_by_pubkey(txn, chat_msg.sender).ok();
        other_chat_bubble(ui, chat_msg, get_display_name(profile.as_ref()), msg_type);
    });
}

fn conversation_composer(
    ui: &mut egui::Ui,
    state: &mut ConversationState,
    conversation_id: ConversationId,
    i18n: &mut Localization,
) -> ComposerResponse {
    {
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, ui.visuals().panel_fill);
    }
    let margin = Margin::symmetric(16, 4);
    let mut action = None;
    let mut composer_has_focus = false;
    Frame::new().inner_margin(margin).show(ui, |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            // TODO(kernelkind): ideally this will be multiline, but the default multiline impl doesn't work the way
            // signal's multiline works... TBC

            let old = mut_visuals_corner_radius(ui, CornerRadius::same(16));

            let hint_text = RichText::new(tr!(
                i18n,
                "Type a message",
                "Placeholder text for the message composer in chats"
            ))
            .color(ui.visuals().noninteractive().fg_stroke.color);
            let mut send = false;
            let is_narrow = is_narrow(ui.ctx());
            let send_button_section = if is_narrow { 32.0 } else { 0.0 };

            StripBuilder::new(ui)
                .size(Size::remainder())
                .size(Size::exact(send_button_section))
                .horizontal(|mut strip| {
                    strip.cell(|ui| {
                        let spacing = ui.spacing().item_spacing.x;
                        let text_height = ui.spacing().item_spacing.y * 1.4;
                        let text_width = (ui.available_width() - spacing).max(0.0);
                        let size = vec2(text_width, text_height);

                        let text_edit = TextEdit::singleline(&mut state.composer)
                            .margin(Margin::symmetric(16, 8))
                            .vertical_align(Align::Center)
                            .desired_width(text_width)
                            .hint_text(hint_text)
                            .min_size(size);
                        let text_resp = ui.add(text_edit);
                        restore_widgets_corner_rad(ui, old);
                        send = text_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        include_input(ui, &text_resp);
                        composer_has_focus = text_resp.has_focus();
                    });

                    if is_narrow {
                        strip.cell(|ui| {
                            ui.add_space(6.0);
                            if ui
                                .add_enabled(
                                    !state.composer.is_empty(),
                                    egui::Button::new("Send").frame(false),
                                )
                                .clicked()
                            {
                                send = true;
                            }
                        });
                    } else {
                        strip.empty();
                    }
                });
            if send {
                action = prepare_send_action(conversation_id, state);
            }
        });
    });

    ComposerResponse {
        action,
        composer_has_focus,
    }
}

struct ComposerResponse {
    action: Option<MessagesAction>,
    composer_has_focus: bool,
}

fn prepare_send_action(
    conversation_id: ConversationId,
    state: &mut ConversationState,
) -> Option<MessagesAction> {
    if state.composer.trim().is_empty() {
        return None;
    }

    let message = std::mem::take(&mut state.composer);
    Some(MessagesAction::SendMessage {
        conversation_id,
        content: message,
    })
}

fn chat_bubble<R>(
    ui: &mut egui::Ui,
    msg_type: MessageType,
    is_self: bool,
    bubble_fill: Color32,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let d = 18;
    let i = 4;

    let (inner_top, inner_bottom) = match msg_type {
        MessageType::Standalone => (d, d),
        MessageType::FirstInSeries => (d, i),
        MessageType::MiddleInSeries => (i, i),
        MessageType::LastInSeries => (i, d),
    };

    let corner_radius = if is_self {
        CornerRadius {
            nw: d,
            ne: inner_top,
            sw: d,
            se: inner_bottom,
        }
    } else {
        CornerRadius {
            nw: inner_top,
            ne: d,
            sw: inner_bottom,
            se: d,
        }
    };

    Frame::new()
        .fill(bubble_fill)
        .corner_radius(corner_radius)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width() * 0.9);
            contents(ui)
        })
        .inner
}

fn self_chat_bubble(
    ui: &mut egui::Ui,
    message: &str,
    msg_type: MessageType,
    timestamp: u64,
) -> egui::Response {
    let bubble_fill = ui.visuals().selection.bg_fill;
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        chat_bubble(ui, msg_type, true, bubble_fill, |ui| {
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                ui.label(RichText::new(message).color(ui.visuals().text_color()));

                if msg_type == MessageType::Standalone || msg_type == MessageType::LastInSeries {
                    let timestamp_label =
                        format_timestamp_label(&local_datetime_from_nostr(timestamp));
                    ui.label(
                        RichText::new(timestamp_label)
                            .small()
                            .color(ui.visuals().window_fill),
                    );
                }
            })
        })
        .inner
    })
    .response
}

fn other_chat_bubble(
    ui: &mut egui::Ui,
    chat_msg: Nip17ChatMessage,
    sender_name: NostrName,
    msg_type: MessageType,
) -> egui::Response {
    let message = chat_msg.message;
    let bubble_fill = ui.visuals().extreme_bg_color;
    let text_color = ui.visuals().text_color();
    let secondary_color = ui.visuals().weak_text_color();

    chat_bubble(ui, msg_type, false, bubble_fill, |ui| {
        ui.vertical(|ui| {
            if msg_type == MessageType::FirstInSeries || msg_type == MessageType::Standalone {
                ui.label(
                    RichText::new(sender_name.name())
                        .strong()
                        .color(secondary_color),
                );
                ui.add_space(2.0);
            }

            ui.with_layout(
                Layout::left_to_right(Align::Max).with_main_wrap(true),
                |ui| {
                    ui.label(RichText::new(message).color(text_color));
                    if msg_type == MessageType::Standalone || msg_type == MessageType::LastInSeries
                    {
                        ui.add_space(6.0);
                        let timestamp_label =
                            format_timestamp_label(&local_datetime_from_nostr(chat_msg.created_at));
                        ui.add(
                            egui::Label::new(
                                RichText::new(timestamp_label)
                                    .small()
                                    .color(secondary_color),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    }
                },
            );
        })
        .response
    })
}

/// An unfortunate hack to change the corner radius of a TextEdit...
/// returns old `CornerRadius`
fn mut_visuals_corner_radius(ui: &mut egui::Ui, rad: CornerRadius) -> WidgetsCornerRadius {
    let widgets = &ui.visuals().widgets;
    let old = WidgetsCornerRadius {
        active: widgets.active.corner_radius,
        hovered: widgets.hovered.corner_radius,
        inactive: widgets.inactive.corner_radius,
        noninteractive: widgets.noninteractive.corner_radius,
        open: widgets.open.corner_radius,
    };

    let widgets = &mut ui.visuals_mut().widgets;
    widgets.active.corner_radius = rad;
    widgets.hovered.corner_radius = rad;
    widgets.inactive.corner_radius = rad;
    widgets.noninteractive.corner_radius = rad;
    widgets.open.corner_radius = rad;

    old
}

fn restore_widgets_corner_rad(ui: &mut egui::Ui, old: WidgetsCornerRadius) {
    let widgets = &mut ui.visuals_mut().widgets;

    widgets.active.corner_radius = old.active;
    widgets.hovered.corner_radius = old.hovered;
    widgets.inactive.corner_radius = old.inactive;
    widgets.noninteractive.corner_radius = old.noninteractive;
    widgets.open.corner_radius = old.open;
}

struct WidgetsCornerRadius {
    active: CornerRadius,
    hovered: CornerRadius,
    inactive: CornerRadius,
    noninteractive: CornerRadius,
    open: CornerRadius,
}

fn format_day_heading(date: NaiveDate, today: &NaiveDate, i18n: &mut Localization) -> String {
    if date == *today {
        tr!(
            i18n,
            "Today",
            "Label shown between chat messages for the current day"
        )
    } else if date == *today - Duration::days(1) {
        tr!(
            i18n,
            "Yesterday",
            "Label shown between chat messages for the previous day"
        )
    } else {
        date.format("%A, %B %-d, %Y").to_string()
    }
}

pub fn format_time_short(
    today: NaiveDate,
    time: &DateTime<Local>,
    i18n: &mut Localization,
) -> String {
    let d = time.date_naive();

    if d == today {
        return format_timestamp_label(time);
    } else if d == today - Duration::days(1) {
        return tr!(
            i18n,
            "Yest",
            "Abbreviated version of yesterday used in conversation summaries"
        );
    }

    let days_ago = today.signed_duration_since(d).num_days();

    if days_ago < 7 {
        return d.format("%a").to_string();
    }

    d.format("%b %-d").to_string()
}

fn format_timestamp_label(dt: &DateTime<Local>) -> String {
    dt.format("%-I:%M %p").to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn conversation_ui(
    cache: &ConversationCache,
    states: &mut ConversationStates,
    jobs: &MediaJobSender,
    ndb: &Ndb,
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    i18n: &mut Localization,
    selected_pubkey: &Pubkey,
) -> Option<MessagesAction> {
    let Some(id) = cache.active else {
        title_label(
            ui,
            &tr!(
                i18n,
                "No conversations yet",
                "label describing that there are no conversations yet",
            ),
        );
        return None;
    };

    let Some(conversation) = cache.get(id) else {
        tracing::error!("could not find active convo id {id}");
        return None;
    };

    let state = states.get_or_insert(id);

    ConversationUi::new(conversation, state, ndb, jobs, img_cache, i18n).ui(ui, selected_pubkey)
}
