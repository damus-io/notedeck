use enostr::{NoteId, Pubkey, RelayId};
use nostr::RelayUrl;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use tracing::error;

use crate::{Accounts, RelayType, RemoteApi};

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
    SummarizeThread(NoteId),
    BookmarkNote,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ContextSelection {
    pub note_key: NoteKey,
    pub action: NoteContextSelection,
}

/// Sanitizes a raw relay string into a relay hint usable in a NIP-19 `nevent`,
/// or `None` if it isn't a real websocket relay.
///
/// Notes ingested over the local network carry the placeholder `multicast`
/// relay (see `enostr`'s `MulticastRelayCache`), which is not a real relay.
/// Parsing through [`RelayUrl`] drops it — and any other malformed or
/// non-`ws`/`wss` entry — while normalizing the URL, so placeholder relays
/// never leak into shareable `nevent` links.
fn sanitize_relay_hint(url: &str) -> Option<String> {
    RelayUrl::parse(url).ok().map(|url| url.to_string())
}

/// Collects relay URLs where the note was actually observed, keeping only valid
/// websocket relays so placeholder relays don't end up in the encoded nevent.
fn relay_hints_for_note(note: &Note<'_>, txn: &Transaction) -> Vec<String> {
    note.relays(txn).filter_map(sanitize_relay_hint).collect()
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
        remote: &mut RemoteApi,
        txn: &Transaction,
        accounts: &Accounts,
    ) {
        match self {
            NoteContextSelection::Broadcast(context) => {
                tracing::info!("Broadcasting note {}", hex::encode(note.id()));
                let relays = match context {
                    BroadcastContext::LocalNetwork => RelayType::Explicit(vec![RelayId::Multicast]),
                    BroadcastContext::Everywhere => RelayType::AccountsWrite,
                };
                remote.publisher(accounts).publish_note(note, relays);
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
                    super::publish::send_unmute_event(
                        ndb,
                        txn,
                        &mut remote.publisher(accounts),
                        kp,
                        &muted,
                        &target,
                    );
                } else {
                    super::publish::send_mute_event(
                        ndb,
                        txn,
                        &mut remote.publisher(accounts),
                        kp,
                        &muted,
                        &target,
                    );
                }
            }
            NoteContextSelection::ReportUser => {}
            NoteContextSelection::SummarizeThread(_) => {
                // Handled at Chrome level — routed to Dave
            }
            NoteContextSelection::BookmarkNote => {
                let target = NoteId::new(*note.id());
                let Some(kp) = accounts.get_selected_account().key.to_full() else {
                    return;
                };
                let bookmarks = accounts.bookmarks();
                if bookmarks.is_bookmarked(target.bytes()) {
                    super::publish::send_unbookmark_event(
                        ndb,
                        txn,
                        &mut remote.publisher(accounts),
                        kp,
                        &bookmarks,
                        &target,
                    );
                } else {
                    super::publish::send_bookmark_event(
                        ndb,
                        txn,
                        &mut remote.publisher(accounts),
                        kp,
                        &bookmarks,
                        &target,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_relay_hint;

    #[test]
    fn keeps_websocket_relay_urls() {
        assert_eq!(
            sanitize_relay_hint("wss://relay.damus.io").as_deref(),
            Some("wss://relay.damus.io")
        );
        assert_eq!(
            sanitize_relay_hint("ws://localhost:7777").as_deref(),
            Some("ws://localhost:7777")
        );
    }

    #[test]
    fn drops_multicast_placeholder() {
        assert_eq!(sanitize_relay_hint("multicast"), None);
    }

    #[test]
    fn drops_non_websocket_urls() {
        assert_eq!(sanitize_relay_hint("https://damus.io"), None);
        assert_eq!(sanitize_relay_hint(""), None);
    }
}
