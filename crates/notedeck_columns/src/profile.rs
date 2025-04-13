use enostr::{FullKeypair, Keypair, Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, ProfileRecord, Tag, Transaction};
use std::collections::HashMap;

use tracing::info;

use crate::{
    profile_state::ProfileState,
    route::{Route, Router},
};

pub struct NostrName<'a> {
    pub username: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub nip05: Option<&'a str>,
}

impl<'a> NostrName<'a> {
    pub fn name(&self) -> &'a str {
        if let Some(name) = self.username {
            name
        } else if let Some(name) = self.display_name {
            name
        } else {
            self.nip05.unwrap_or("??")
        }
    }

    pub fn unknown() -> Self {
        Self {
            username: None,
            display_name: None,
            nip05: None,
        }
    }
}

fn is_empty(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}

pub fn get_display_name<'a>(record: Option<&ProfileRecord<'a>>) -> NostrName<'a> {
    if let Some(record) = record {
        if let Some(profile) = record.record().profile() {
            let display_name = profile.display_name().filter(|n| !is_empty(n));
            let username = profile.name().filter(|n| !is_empty(n));
            let nip05 = if let Some(raw_nip05) = profile.nip05() {
                if let Some(at_pos) = raw_nip05.find('@') {
                    if raw_nip05.starts_with('_') {
                        raw_nip05.get(at_pos + 1..)
                    } else {
                        Some(raw_nip05)
                    }
                } else {
                    None
                }
            } else {
                None
            };

            NostrName {
                username,
                display_name,
                nip05,
            }
        } else {
            NostrName::unknown()
        }
    } else {
        NostrName::unknown()
    }
}

pub fn is_following(ndb: &Ndb, txn: &Transaction, own_key: Pubkey, target_key: Pubkey) -> bool {
    ndb.query(txn, &[follows_filter(own_key)], 1)
        .ok()
        .map_or(false, |results| {
            results.first().map_or(false, |result| {
                result.note.tags().iter().filter(p_tags()).any(|tag| {
                    tag.get_unchecked(1)
                        .variant()
                        .id()
                        .map_or(false, |tag_key| tag_key == target_key.bytes())
                })
            })
        })
}

fn p_tags() -> fn(&Tag) -> bool {
    |tag| tag.count() > 0 && tag.get_unchecked(0).variant().str() == Some("p")
}

fn follows_filter(pubkey: Pubkey) -> Filter {
    Filter::new()
        .authors([pubkey.bytes()])
        .kinds([3])
        .limit(1)
        .build()
}

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
    Follow(Keypair, Pubkey),
    Unfollow(Keypair, Pubkey),
}

impl ProfileAction {
    pub fn process(
        &self,
        state_map: &mut HashMap<Pubkey, ProfileState>,
        ndb: &Ndb,
        pool: &mut RelayPool,
        router: &mut Router<Route>,
    ) {
        match self {
            ProfileAction::Edit(kp) => {
                router.route_to(Route::EditProfile(kp.pubkey));
            }
            ProfileAction::SaveChanges(changes) => {
                let raw_msg = format!("[\"EVENT\",{}]", changes.to_note().json().unwrap());

                let _ = ndb.process_event_with(
                    raw_msg.as_str(),
                    nostrdb::IngestMetadata::new().client(true),
                );
                let _ = state_map.remove_entry(&changes.kp.pubkey);

                info!("sending {}", raw_msg);
                pool.send(&enostr::ClientMessage::raw(raw_msg));

                router.go_back();
            }
            ProfileAction::Follow(keypair, target_key) => {
                Self::send_follow_user_event(ndb, pool, keypair, target_key);
            }
            ProfileAction::Unfollow(keypair, target_key) => {
                Self::send_unfollow_user_event(ndb, pool, keypair, target_key);
            }
        }
    }

    fn send_follow_user_event(
        ndb: &Ndb,
        pool: &mut RelayPool,
        keypair: &Keypair,
        target_key: &Pubkey,
    ) {
        Self::send_kind_3_event(ndb, pool, keypair, target_key, true);
    }

    fn send_unfollow_user_event(
        ndb: &Ndb,
        pool: &mut RelayPool,
        keypair: &Keypair,
        target_key: &Pubkey,
    ) {
        Self::send_kind_3_event(ndb, pool, keypair, target_key, false);
    }

    fn send_kind_3_event(
        ndb: &Ndb,
        pool: &mut RelayPool,
        keypair: &Keypair,
        target_key: &Pubkey,
        is_follow: bool,
    ) {
        let txn = Transaction::new(ndb).expect("txn");
        let follows_filter = follows_filter(keypair.pubkey);
        let mut following_list = Vec::new();
        if let Ok(results) = ndb.query(&txn, &[follows_filter], 1) {
            if let Some(result) = results.first() {
                result
                    .note
                    .tags()
                    .iter()
                    .filter(p_tags())
                    .map(|tag| tag.get_unchecked(1).variant().id().unwrap())
                    .for_each(|key| following_list.push(key));
            }
        }
        if is_follow {
            following_list.push(target_key.bytes());
        } else if let Some(index) = following_list
            .iter()
            .position(|key| *key == target_key.bytes())
        {
            following_list.remove(index);
        }

        let mut builder = NoteBuilder::new().kind(3);
        for pk in following_list {
            builder = builder.start_tag().tag_str("p").tag_str(&hex::encode(pk));
        }

        let note = builder
            .content("")
            .sign(&keypair.secret_key.clone().unwrap().to_secret_bytes())
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
}
