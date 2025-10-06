use enostr::{ClientMessage, NoteId, Pubkey, RelayPool};
use nostrdb::{Note, NoteKey};
use tracing::error;

/// When broadcasting notes, this determines whether to broadcast
/// over the local network via multicast, or globally
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BroadcastContext {
    LocalNetwork,
    Everywhere,
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum NoteContextSelection {
    CopyText,
    CopyPubkey,
    CopyNoteId,
    CopyNoteJSON,
    Broadcast(BroadcastContext),
    CopyLink,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ContextSelection {
    pub note_key: NoteKey,
    pub action: NoteContextSelection,
}

impl NoteContextSelection {
    pub fn process_selection(
        &self,
        ui: &mut egui::Ui,
        note: &Note<'_>,
        pool: &mut RelayPool,
        note_author_is_selected_acc: bool,
    ) {
        match self {
            NoteContextSelection::Broadcast(context) => {
                tracing::info!("Broadcasting note {}", hex::encode(note.id()));
                match context {
                    BroadcastContext::LocalNetwork => {
                        pool.send_to(&ClientMessage::event(note).unwrap(), "multicast");
                    }

                    BroadcastContext::Everywhere => {
                        pool.send(&ClientMessage::event(note).unwrap());
                    }
                }
            }
            NoteContextSelection::CopyText => {
                ui.ctx().copy_text(note.content().to_string());
            }
            NoteContextSelection::CopyPubkey => {
                if let Some(bech) = Pubkey::new(*note.pubkey()).npub() {
                    ui.ctx().copy_text(bech);
                }
            }
            NoteContextSelection::CopyNoteId => {
                if let Some(bech) = NoteId::new(*note.id()).to_bech() {
                    ui.ctx().copy_text(bech);
                }
            }
            NoteContextSelection::CopyNoteJSON => match note.json() {
                Ok(json) => ui.ctx().copy_text(json),
                Err(err) => error!("error copying note json: {err}"),
            },
            NoteContextSelection::CopyLink => {
                let damus_url = |s| format!("https://damus.io/{s}");
                if note_author_is_selected_acc {
                    let nip19event = nostr::nips::nip19::Nip19Event::new(
                        nostr::event::EventId::from_byte_array(*note.id()),
                        pool.urls(),
                    );
                    let Ok(bech) = nostr::nips::nip19::ToBech32::to_bech32(&nip19event) else {
                        return;
                    };
                    ui.ctx().copy_text(damus_url(bech));
                } else {
                    let Some(bech) = NoteId::new(*note.id()).to_bech() else {
                        return;
                    };

                    ui.ctx().copy_text(damus_url(bech));
                }
            }
        }
    }
}
