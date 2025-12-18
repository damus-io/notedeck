use std::borrow::Cow;

use chrono::{DateTime, Local, Utc};
use egui::{Layout, RichText};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    name::get_display_name, tr, tr_plural, Images, Localization, MediaJobSender, NoteRef,
    NotedeckTextStyle,
};
use notedeck_ui::ProfilePic;

use crate::cache::{Conversation, ConversationCache, ConversationMetadata};

pub mod convo;

#[derive(Clone, Debug)]
pub struct ConversationSummary<'a> {
    pub metadata: &'a ConversationMetadata,
    pub last_message: Option<&'a NoteRef>,
    pub unread: bool,
    pub total_messages: usize,
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
) {
    let Some(conversation) = cache.get_active() else {
        title_label(
            ui,
            &tr!(
                i18n,
                "Conversation",
                "Title used when viewing an unknown conversation"
            ),
        );
        return;
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

    conversation_header(ui, &title, jobs, img_cache, true, partner_profile.as_ref());
}

fn title_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Label::new(RichText::new(text).text_style(NotedeckTextStyle::Heading.text_style()))
            .selectable(false),
    )
}

pub fn conversation_header(
    ui: &mut egui::Ui,
    title: &str,
    jobs: &MediaJobSender,
    img_cache: &mut Images,
    show_partner_avatar: bool,
    partner_profile: Option<&ProfileRecord<'_>>,
) {
    ui.with_layout(
        Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
        |ui| {
            if show_partner_avatar {
                let mut pic = ProfilePic::from_profile_or_default(img_cache, jobs, partner_profile)
                    .size(ProfilePic::medium_size() as f32);
                ui.add(&mut pic);
                ui.add_space(8.0);
            }

            ui.heading(title);
        },
    );
}
