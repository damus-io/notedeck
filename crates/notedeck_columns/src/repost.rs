use enostr::{Keypair, NoteId, RelayId};
use nostrdb::{Ndb, Note, NoteBuilder, Transaction};
use notedeck::{Accounts, RelayType, RemoteApi};

use crate::{nav::RouterAction, Route};

pub fn generate_repost_event<'a>(
    ndb: &'a Ndb,
    noteid_to_repost: &NoteId,
    signer_nsec: &[u8; 32],
    accounts: &Accounts,
) -> Result<Note<'a>, String> {
    let txn = Transaction::new(ndb).expect("txn");
    let note_to_repost = ndb
        .get_note_by_id(&txn, noteid_to_repost.bytes())
        .map_err(|e| format!("could not find note to repost {noteid_to_repost:?}: {e}"))?;

    if note_to_repost.kind() != 1 {
        return Err(format!(
            "trying to generate a kind 6 repost but the kind is not 1 (it's {})",
            note_to_repost.kind()
        ));
    }

    let urls: Vec<String> = accounts
        .selected_account_write_relays()
        .into_iter()
        .filter_map(|r| match r {
            RelayId::Websocket(url) => Some(url.to_string()),
            RelayId::Multicast => None,
        })
        .collect();
    let Some(relay) = urls.first() else {
        return Err(
            "relay pool does not have any relays. This makes meeting the repost spec impossible"
                .to_owned(),
        );
    };

    let note_to_repost_content = note_to_repost
        .json()
        .map_err(|e| format!("could not convert note {note_to_repost:?} to json: {e}"))?;

    NoteBuilder::new()
        .content(&note_to_repost_content)
        .kind(6)
        .start_tag()
        .tag_str("e")
        .tag_id(note_to_repost.id())
        .tag_str(relay)
        .start_tag()
        .tag_str("p")
        .tag_id(note_to_repost.pubkey())
        .sign(signer_nsec)
        .build()
        .ok_or("Failure in NoteBuilder::build".to_owned())
}

pub enum RepostAction {
    Kind06Repost(NoteId),
    Quote(NoteId),
    Cancel,
}

impl RepostAction {
    pub fn process(
        self,
        ndb: &nostrdb::Ndb,
        current_user: &Keypair,
        accounts: &Accounts,
        remote: &mut RemoteApi<'_>,
    ) -> Option<RouterAction> {
        match self {
            RepostAction::Quote(note_id) => {
                Some(RouterAction::CloseSheetThenRoute(Route::quote(note_id)))
            }
            RepostAction::Kind06Repost(note_id) => {
                let Some(full_user) = current_user.to_full() else {
                    tracing::error!("Attempting to make a kind 6 repost, but we don't have nsec");
                    return None;
                };

                let repost_ev = generate_repost_event(
                    ndb,
                    &note_id,
                    &full_user.secret_key.secret_bytes(),
                    accounts,
                )
                .inspect_err(|e| tracing::error!("failure to generate repost event: {e}"))
                .ok()?;

                let Ok(event) = &enostr::ClientMessage::event(&repost_ev) else {
                    tracing::error!("send_note_builder: failed to build json");
                    return None;
                };

                let Ok(json) = event.to_json() else {
                    tracing::error!("send_note_builder: failed to build json");
                    return None;
                };

                let _ = ndb.process_event_with(&json, nostrdb::IngestMetadata::new().client(true));

                let mut publisher = remote.publisher(accounts);
                publisher.publish_note(&repost_ev, RelayType::AccountsWrite);

                Some(RouterAction::GoBack)
            }
            RepostAction::Cancel => Some(RouterAction::GoBack),
        }
    }
}
