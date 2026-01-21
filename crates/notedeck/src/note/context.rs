use enostr::{ClientMessage, NoteId, Pubkey, RelayPool};
use nostrdb::{Note, NoteKey, Transaction};
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
    CopyNevent,
    CopyNoteJSON,
    Broadcast(BroadcastContext),
    CopyNeventLink,
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

/// Check if a kind is a parameterized replaceable event (30000-39999)
fn is_parameterized_replaceable(kind: u32) -> bool {
    (30000..40000).contains(&kind)
}

/// Get the 'd' tag value from a note (used for parameterized replaceable events)
fn get_d_tag<'a>(note: &'a Note<'a>) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() >= 2 && tag.get_str(0) == Some("d") {
            return tag.get_str(1);
        }
    }
    None
}

/// Create a bech32 naddr for parameterized replaceable events
fn note_nip19_addr_bech(note: &Note<'_>, txn: &Transaction) -> Option<String> {
    use nostr::nips::nip01::Coordinate;
    use nostr::nips::nip19::ToBech32;

    let kind = nostr::Kind::from(note.kind() as u16);
    let public_key = nostr::PublicKey::from_slice(note.pubkey()).ok()?;
    let identifier = get_d_tag(note).unwrap_or("");

    let relay_hints = relay_hints_for_note(note, txn);
    let relays: Vec<nostr::RelayUrl> = relay_hints
        .iter()
        .filter_map(|r| nostr::RelayUrl::parse(r).ok())
        .collect();

    let mut coord = Coordinate::new(kind, public_key).identifier(identifier);
    coord.relays = relays;

    coord.to_bech32().ok()
}

/// Create a bech32 nevent for regular events
fn note_nip19_event_bech(note: &Note<'_>, txn: &Transaction) -> Option<String> {
    let relay_hints = relay_hints_for_note(note, txn);
    let nip19event = nostr::nips::nip19::Nip19Event::new(
        nostr::event::EventId::from_byte_array(*note.id()),
        relay_hints,
    );

    nostr::nips::nip19::ToBech32::to_bech32(&nip19event).ok()
}

/// Get the appropriate bech32 identifier for a note (naddr for parameterized replaceable, nevent otherwise)
fn note_bech32(note: &Note<'_>, txn: &Transaction) -> Option<String> {
    if is_parameterized_replaceable(note.kind()) {
        note_nip19_addr_bech(note, txn)
    } else {
        note_nip19_event_bech(note, txn)
    }
}

impl NoteContextSelection {
    pub fn process_selection(
        &self,
        ui: &mut egui::Ui,
        note: &Note<'_>,
        pool: &mut RelayPool,
        txn: &Transaction,
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
                // Use naddr for parameterized replaceable events (30000-39999), nevent otherwise
                if let Some(bech) = note_bech32(note, txn) {
                    ui.ctx().copy_text(bech);
                }
            }
            NoteContextSelection::CopyNoteJSON => match note.json() {
                Ok(json) => ui.ctx().copy_text(json),
                Err(err) => error!("error copying note json: {err}"),
            },
            NoteContextSelection::CopyNeventLink => {
                let damus_url = |s| format!("https://damus.io/{s}");
                // Use naddr for parameterized replaceable events (30000-39999), nevent otherwise
                if let Some(bech) = note_bech32(note, txn) {
                    ui.ctx().copy_text(damus_url(bech));
                    return;
                }

                // Fallback to event id without relay hints if encoding fails.
                if let Some(bech) = NoteId::new(*note.id()).to_bech() {
                    ui.ctx().copy_text(damus_url(bech));
                }
            }
        }
    }
}
