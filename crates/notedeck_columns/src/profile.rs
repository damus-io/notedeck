use std::collections::HashMap;

use enostr::{FullKeypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, NoteBuildOptions, NoteBuilder};

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
}
