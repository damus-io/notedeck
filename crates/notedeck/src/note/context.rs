use enostr::{ClientMessage, NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use tracing::error;

use crate::Accounts;

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
    CopyNevent,
    CopyNoteJSON,
    Broadcast(BroadcastContext),
    CopyNeventLink,
    MuteUser,
    ReportUser,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ContextSelection {
    pub note_key: NoteKey,
    pub action: NoteContextSelection,
}

/// Collects relay URLs where the note was actually observed.
fn relay_hints_for_note(note: &Note<'_>, txn: &Transaction) -> Vec<String> {
    note.relays(txn).map(|relay| relay.to_owned()).collect()
}

fn note_nip19_event_bech(note: &Note<'_>, txn: &Transaction) -> Option<String> {
    let relay_hints = relay_hints_for_note(note, txn);
    let nip19event = nostr::nips::nip19::Nip19Event::new(
        nostr::event::EventId::from_byte_array(*note.id()),
        relay_hints,
    );

    nostr::nips::nip19::ToBech32::to_bech32(&nip19event).ok()
}

impl NoteContextSelection {
    pub fn process_selection(
        &self,
        ui: &mut egui::Ui,
        note: &Note<'_>,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        accounts: &Accounts,
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
            NoteContextSelection::CopyNevent => {
                if let Some(bech) = note_nip19_event_bech(note, txn) {
                    ui.ctx().copy_text(bech);
                }
            }
            NoteContextSelection::CopyNoteJSON => match note.json() {
                Ok(json) => ui.ctx().copy_text(json),
                Err(err) => error!("error copying note json: {err}"),
            },
            NoteContextSelection::CopyNeventLink => {
                let damus_url = |s| format!("https://damus.io/{s}");
                if let Some(bech) = note_nip19_event_bech(note, txn) {
                    ui.ctx().copy_text(damus_url(bech));
                    return;
                }

                // Fallback to event id without relay hints if encoding fails.
                if let Some(bech) = NoteId::new(*note.id()).to_bech() {
                    ui.ctx().copy_text(damus_url(bech));
                }
            }
            NoteContextSelection::MuteUser => {
                let target = Pubkey::new(*note.pubkey());
                let Some(kp) = accounts.get_selected_account().key.to_full() else {
                    return;
                };
                let muted = accounts.mute();
                if muted.is_pk_muted(target.bytes()) {
                    super::publish::send_unmute_event(ndb, txn, pool, kp, &muted, &target);
                } else {
                    super::publish::send_mute_event(ndb, txn, pool, kp, &muted, &target);
                }
            }
            NoteContextSelection::ReportUser => {}
        }
    }
}
