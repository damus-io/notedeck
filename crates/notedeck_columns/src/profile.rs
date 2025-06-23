use std::collections::HashMap;

use enostr::{FilledKeypair, FullKeypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};

use notedeck::{Accounts, ContactState};
use tracing::info;

use crate::{nav::RouterAction, profile_state::ProfileState, route::Route};

pub struct SaveProfileChanges {
    pub kp: FullKeypair,
    pub state: ProfileState,
}

impl SaveProfileChanges {
    pub fn new(kp: FullKeypair, state: ProfileState) -> Self {
        Self { kp, state }
    }
    pub fn to_note(&self) -> Note {
        let sec = &self.kp.secret_key.to_secret_bytes();
        add_client_tag(NoteBuilder::new())
            .kind(0)
            .content(&self.state.to_json())
            .options(NoteBuildOptions::default().created_at(true).sign(sec))
            .build()
            .expect("should build")
    }
}

fn add_client_tag(builder: NoteBuilder<'_>) -> NoteBuilder<'_> {
    builder
        .start_tag()
        .tag_str("client")
        .tag_str("Damus Notedeck")
}

pub enum ProfileAction {
    Edit(FullKeypair),
    SaveChanges(SaveProfileChanges),
}

impl ProfileAction {
    pub fn process(
        &self,
        state_map: &mut HashMap<Pubkey, ProfileState>,
        ndb: &Ndb,
        pool: &mut RelayPool,
    ) -> Option<RouterAction> {
        match self {
            ProfileAction::Edit(kp) => Some(RouterAction::route_to(Route::EditProfile(kp.pubkey))),
            ProfileAction::SaveChanges(changes) => {
                let raw_msg = format!("[\"EVENT\",{}]", changes.to_note().json().unwrap());

                let _ = ndb.process_event_with(
                    raw_msg.as_str(),
                    nostrdb::IngestMetadata::new().client(true),
                );
                let _ = state_map.remove_entry(&changes.kp.pubkey);

                info!("sending {}", raw_msg);
                pool.send(&enostr::ClientMessage::raw(raw_msg));

                Some(RouterAction::GoBack)
            }
        }
    }

    fn send_follow_user_event(
        ndb: &Ndb,
        pool: &mut RelayPool,
        accounts: &Accounts,
        target_key: &Pubkey,
    ) {
        send_kind_3_event(ndb, pool, accounts, FollowAction::Follow(target_key));
    }

    fn send_unfollow_user_event(
        ndb: &Ndb,
        pool: &mut RelayPool,
        accounts: &Accounts,
        target_key: &Pubkey,
    ) {
        send_kind_3_event(ndb, pool, accounts, FollowAction::Unfollow(target_key));
    }
}

pub fn builder_from_note<F>(note: Note<'_>, skip_tag: Option<F>) -> NoteBuilder<'_>
where
    F: Fn(&nostrdb::Tag<'_>) -> bool,
{
    let mut builder = NoteBuilder::new();

    builder = builder.content(note.content());
    builder = builder.options(NoteBuildOptions::default());
    builder = builder.kind(note.kind());
    builder = builder.pubkey(note.pubkey());

    for tag in note.tags() {
        if let Some(skip) = &skip_tag {
            if skip(&tag) {
                continue;
            }
        }

        builder = builder.start_tag();
        for tag_item in tag {
            builder = match tag_item.variant() {
                nostrdb::NdbStrVariant::Id(i) => builder.tag_id(i),
                nostrdb::NdbStrVariant::Str(s) => builder.tag_str(s),
            };
        }
    }

    builder
}

enum FollowAction<'a> {
    Follow(&'a Pubkey),
    Unfollow(&'a Pubkey),
}

fn send_kind_3_event(ndb: &Ndb, pool: &mut RelayPool, accounts: &Accounts, action: FollowAction) {
    let Some(kp) = accounts.get_selected_account().key.to_full() else {
        return;
    };

    let txn = Transaction::new(ndb).expect("txn");

    let ContactState::Received {
        contacts: _,
        note_key,
    } = accounts.get_selected_account().data.contacts.get_state()
    else {
        return;
    };

    let contact_note = match ndb.get_note_by_key(&txn, *note_key).ok() {
        Some(n) => n,
        None => {
            tracing::error!("Somehow we are in state ContactState::Received but the contact note key doesn't exist");
            return;
        }
    };

    if contact_note.kind() != 3 {
        tracing::error!("Something very wrong just occured. The key for the supposed contact note yielded a note which was not a contact...");
        return;
    }

    let builder = match action {
        FollowAction::Follow(pubkey) => {
            builder_from_note(contact_note, None::<fn(&nostrdb::Tag<'_>) -> bool>)
                .start_tag()
                .tag_str("p")
                .tag_str(&pubkey.hex())
        }
        FollowAction::Unfollow(pubkey) => builder_from_note(
            contact_note,
            Some(|tag: &nostrdb::Tag<'_>| {
                if tag.count() < 2 {
                    return false;
                }

                let Some("p") = tag.get_str(0) else {
                    return false;
                };

                let Some(cur_val) = tag.get_id(1) else {
                    return false;
                };

                cur_val == pubkey.bytes()
            }),
        ),
    };

    send_note_builder(builder, ndb, pool, kp);
}

fn send_note_builder(builder: NoteBuilder, ndb: &Ndb, pool: &mut RelayPool, kp: FilledKeypair) {
    let note = builder
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("build note");

    let raw_msg = format!("[\"EVENT\",{}]", note.json().unwrap());

    let _ = ndb.process_event_with(
        raw_msg.as_str(),
        nostrdb::IngestMetadata::new().client(true),
    );
    info!("sending {}", raw_msg);
    pool.send(&enostr::ClientMessage::raw(raw_msg));
}

fn construct_new_contact_list<'a>(pk: &'a Pubkey) -> NoteBuilder<'a> {
    NoteBuilder::new()
        .content("")
        .kind(3)
        .options(NoteBuildOptions::default())
        .start_tag()
        .tag_str("p")
        .tag_str(&pk.hex())
}
