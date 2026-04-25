use std::borrow::Cow;

use chrono::{DateTime, Local, Utc};
use egui::{vec2, Layout, RichText, Sense};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    name::get_display_name, tr, tr_plural, Images, Localization, MediaJobSender, NoteRef,
    NotedeckTextStyle, DOUBLE_RATCHET_SIG_PREFIX,
};
use notedeck_ui::ProfilePic;

use crate::nav::MessagesAction;

use crate::cache::{Conversation, ConversationCache, ConversationMetadata, ConversationStates};

pub mod convo;
pub mod convo_list;
pub mod create_convo;
pub mod messages;
pub mod nav;

#[derive(Clone, Debug)]
pub struct ConversationSummary<'a> {
    pub metadata: &'a ConversationMetadata,
    pub last_message: Option<&'a NoteRef>,
    pub unread: bool,
    pub total_messages: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MessagesTransportStatus {
    pub double_ratchet_active: bool,
    pub active_conversation_double_ratchet_supported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutgoingTransport {
    Nip17,
    DoubleRatchet,
}

impl MessagesTransportStatus {
    fn outgoing_transport(self, conversation: &Conversation) -> OutgoingTransport {
        if self.double_ratchet_active
            && self.active_conversation_double_ratchet_supported
            && conversation.metadata.participants.len() <= 2
        {
            OutgoingTransport::DoubleRatchet
        } else {
            OutgoingTransport::Nip17
        }
    }
}

impl<'a> ConversationSummary<'a> {
    pub fn new(convo: &'a Conversation, last_read: Option<NoteRef>) -> Self {
        Self {
            metadata: &convo.metadata,
            last_message: convo.messages.latest(),
            unread: last_read.is_some_and(|r| {
                let Some(latest) = convo.messages.latest() else {
                    return false;
                };

                r < *latest
            }),
            total_messages: convo.messages.len(),
        }
    }
}

fn fallback_convo_title(
    participants: &[Pubkey],
    txn: &Transaction,
    ndb: &Ndb,
    current: &Pubkey,
    i18n: &mut Localization,
) -> String {
    let fallback = tr!(
        i18n,
        "Conversation",
        "Fallback title when no direct chat partner is available"
    );
    if participants.is_empty() {
        return fallback;
    }

    let others: Vec<&Pubkey> = participants.iter().filter(|pk| *pk != current).collect();

    if let Some(partner) = direct_chat_partner(participants, current) {
        return participant_label(ndb, txn, partner);
    }

    if others.is_empty() {
        return tr!(
            i18n,
            "Note to Self",
            "Conversation title used when a chat only has the current user"
        );
    }

    let names: Vec<String> = others
        .iter()
        .map(|pk| participant_label(ndb, txn, pk))
        .collect();

    if names.is_empty() {
        return fallback;
    }

    names.join(", ")
}

pub fn conversation_title<'a>(
    metadata: &'a ConversationMetadata,
    txn: &Transaction,
    ndb: &Ndb,
    current: &Pubkey,
    i18n: &mut Localization,
) -> Cow<'a, str> {
    if let Some(title) = metadata.title.as_ref() {
        Cow::Borrowed(title.title.as_str())
    } else {
        Cow::Owned(fallback_convo_title(
            &metadata.participants,
            txn,
            ndb,
            current,
            i18n,
        ))
    }
}

pub fn conversation_meta_line(
    summary: &ConversationSummary<'_>,
    i18n: &mut Localization,
) -> String {
    let mut parts = Vec::new();
    if summary.total_messages > 0 {
        parts.push(tr_plural!(
            i18n,
            "{count} message",
            "{count} messages",
            "Count of messages shown in a chat summary line",
            summary.total_messages,
        ));
    } else {
        parts.push(tr!(
            i18n,
            "No messages yet",
            "Chat summary text when the conversation has no messages"
        ));
    }

    parts.join(" • ")
}

pub fn direct_chat_partner<'a>(participants: &'a [Pubkey], current: &Pubkey) -> Option<&'a Pubkey> {
    if participants.len() != 2 {
        return None;
    }

    participants.iter().find(|pk| *pk != current)
}

pub fn participant_label(ndb: &Ndb, txn: &Transaction, pk: &Pubkey) -> String {
    let record = ndb.get_profile_by_pubkey(txn, pk.bytes()).ok();
    let name = get_display_name(record.as_ref());

    if name.display_name.is_some() || name.username.is_some() {
        name.name().to_owned()
    } else {
        short_pubkey(pk)
    }
}

fn short_pubkey(pk: &Pubkey) -> String {
    let hex = pk.hex();
    const START: usize = 8;
    const END: usize = 4;
    if hex.len() <= START + END {
        hex.to_owned()
    } else {
        format!("{}…{}", &hex[..START], &hex[hex.len() - END..])
    }
}

pub fn local_datetime(day: i64) -> DateTime<Local> {
    DateTime::<Utc>::from_timestamp(day, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap())
        .with_timezone(&Local)
}

pub fn local_datetime_from_nostr(timestamp: u64) -> DateTime<Local> {
    local_datetime(timestamp as i64)
}

pub fn login_nsec_prompt(ui: &mut egui::Ui, i18n: &mut Localization) {
    ui.centered_and_justified(|ui| {
        ui.vertical(|ui| {
            ui.heading(tr!(
                i18n,
                "Add your private key",
                "Heading shown when prompting the user to add a private key to use messages"
            ));
            ui.label(tr!(
                i18n,
                "Messages are end-to-end encrypted. Add your nsec in Accounts to read and send chats.",
                "Description shown under the private key prompt in the Messages view"
            ));
        });
    });
}

pub fn conversation_header_impl(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    cache: &ConversationCache,
    selected_pubkey: &Pubkey,
    ndb: &Ndb,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
) -> Option<MessagesAction> {
    let Some(conversation) = cache.get_active() else {
        title_label(
            ui,
            &tr!(
                i18n,
                "Conversation",
                "Title used when viewing an unknown conversation"
            ),
        );
        return None;
    };

    let txn = Transaction::new(ndb).expect("txn");

    let title = conversation_title(&conversation.metadata, &txn, ndb, selected_pubkey, i18n);
    let summary = ConversationSummary {
        metadata: &conversation.metadata,
        last_message: conversation.messages.latest(),
        unread: false,
        total_messages: conversation.messages.len(),
    };
    let partner = direct_chat_partner(summary.metadata.participants.as_slice(), selected_pubkey);
    let partner_profile = partner.and_then(|pk| ndb.get_profile_by_pubkey(&txn, pk.bytes()).ok());

    let clicked = conversation_header(ui, &title, jobs, img_cache, true, partner_profile.as_ref());
    if clicked {
        partner.map(|pk| MessagesAction::Profile(*pk))
    } else {
        None
    }
}

fn title_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Label::new(RichText::new(text).text_style(NotedeckTextStyle::Heading.text_style()))
            .selectable(false),
    )
}

/// Renders the conversation header. Returns `true` if the pfp or name was clicked.
pub fn conversation_header(
    ui: &mut egui::Ui,
    title: &str,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    show_partner_avatar: bool,
    partner_profile: Option<&ProfileRecord<'_>>,
) -> bool {
    let mut clicked = false;
    ui.with_layout(
        Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
        |ui| {
            if show_partner_avatar {
                let mut pic = ProfilePic::from_profile_or_default(img_cache, jobs, partner_profile)
                    .sense(Sense::click())
                    .size(ProfilePic::medium_size() as f32);
                let pfp_resp = ui.add(&mut pic);
                ui.add_space(8.0);

                let name_resp = ui.add(
                    egui::Label::new(
                        RichText::new(title).text_style(NotedeckTextStyle::Heading.text_style()),
                    )
                    .sense(Sense::click()),
                );

                if pfp_resp.clicked() || name_resp.clicked() {
                    clicked = true;
                }
                if pfp_resp.hovered() || name_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            } else {
                ui.heading(title);
            }
        },
    );
    clicked
}

pub fn conversation_details_tooltip(i18n: &mut Localization) -> String {
    tr!(
        i18n,
        "Chat Details",
        "Tooltip for the chat transport and participant details button"
    )
}

pub fn conversation_details_button(ui: &mut egui::Ui, hover_text: &str) -> egui::Response {
    let size = vec2(32.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if response.hovered() || response.has_focus() {
            ui.painter().rect_filled(rect, 8.0, visuals.weak_bg_fill);
        }

        let color = visuals.fg_stroke.color;
        for offset in [-6.0, 0.0, 6.0] {
            ui.painter()
                .circle_filled(rect.center() + vec2(offset, 0.0), 2.0, color);
        }
    }

    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover_text)
}

pub fn show_conversation_details_modal(
    ui: &mut egui::Ui,
    cache: &ConversationCache,
    states: &mut ConversationStates,
    ndb: &Ndb,
    transport: MessagesTransportStatus,
    i18n: &mut Localization,
) {
    let Some(active) = cache.active else {
        return;
    };
    let Some(conversation) = cache.get(active) else {
        return;
    };
    let state = states.get_or_insert(active);
    if !state.details_open {
        return;
    }

    let mut open = state.details_open;
    egui::Window::new(tr!(
        i18n,
        "Chat Details",
        "Title for chat transport and participant details dialog"
    ))
    .collapsible(false)
    .resizable(false)
    .open(&mut open)
    .show(ui.ctx(), |ui| {
        let outgoing = transport.outgoing_transport(conversation);
        detail_row(
            ui,
            &tr!(
                i18n,
                "Outgoing",
                "Label for the transport used by newly sent chat messages"
            ),
            match outgoing {
                OutgoingTransport::Nip17 => "NIP-17",
                OutgoingTransport::DoubleRatchet => "Double Ratchet",
            },
        );

        let ratchet_status = double_ratchet_status_text(transport, i18n);
        detail_row(
            ui,
            &tr!(
                i18n,
                "Double Ratchet",
                "Label for double-ratchet chat transport status"
            ),
            &ratchet_status,
        );

        if outgoing == OutgoingTransport::DoubleRatchet {
            detail_row(
                ui,
                &tr!(
                    i18n,
                    "Automatic fallback",
                    "Label for whether double-ratchet sends automatically fall back to NIP-17"
                ),
                &tr!(
                    i18n,
                    "None",
                    "Value indicating no automatic fallback is used for double-ratchet chat sends"
                ),
            );
        }

        let counts = conversation_transport_counts(conversation, ndb);
        detail_row(
            ui,
            &tr!(
                i18n,
                "Messages",
                "Label for stored chat message transport counts"
            ),
            &format!(
                "NIP-17 {} / Double Ratchet {}",
                counts.nip17, counts.double_ratchet
            ),
        );

        detail_row(
            ui,
            &tr!(i18n, "Participants", "Label for chat participant count"),
            &conversation.metadata.participants.len().to_string(),
        );
    });
    states.get_or_insert(active).details_open = open;
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());
        ui.add_space(12.0);
        ui.label(value);
    });
}

fn double_ratchet_status_text(
    transport: MessagesTransportStatus,
    i18n: &mut Localization,
) -> String {
    if transport.double_ratchet_active {
        if transport.active_conversation_double_ratchet_supported {
            tr!(
                i18n,
                "peer supported",
                "Double-ratchet status value shown when peer support is known locally"
            )
        } else {
            tr!(
                i18n,
                "not discovered",
                "Double-ratchet status value shown when no peer support is known locally"
            )
        }
    } else {
        tr!(
            i18n,
            "unavailable",
            "Double-ratchet status value shown when no full-key account is active"
        )
    }
}

#[derive(Default)]
struct TransportCounts {
    nip17: usize,
    double_ratchet: usize,
}

fn conversation_transport_counts(conversation: &Conversation, ndb: &Ndb) -> TransportCounts {
    let mut counts = TransportCounts::default();
    let Ok(txn) = Transaction::new(ndb) else {
        return counts;
    };

    for pkg in &conversation.messages.messages_ordered {
        let Ok(note) = ndb.get_note_by_key(&txn, pkg.note_ref.key) else {
            continue;
        };

        if note.sig().starts_with(&DOUBLE_RATCHET_SIG_PREFIX) {
            counts.double_ratchet += 1;
        } else {
            counts.nip17 += 1;
        }
    }

    counts
}
