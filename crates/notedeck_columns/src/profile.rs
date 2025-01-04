use std::collections::HashMap;

use enostr::{Filter, FullKeypair, Pubkey, RelayPool};
use nostrdb::{
    FilterBuilder, Ndb, Note, NoteBuildOptions, NoteBuilder, ProfileRecord, Transaction,
};

use notedeck::{filter::default_limit, FilterState, NoteCache, NoteRef};
use tracing::info;

use crate::{
    multi_subscriber::MultiSubscriber,
    notes_holder::NotesHolder,
    profile_state::ProfileState,
    route::{Route, Router},
    timeline::{copy_notes_into_timeline, PubkeySource, Timeline, TimelineKind, TimelineTab},
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

pub struct Profile {
    pub timeline: Timeline,
    pub multi_subscriber: Option<MultiSubscriber>,
}

impl Profile {
    pub fn new(
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        source: PubkeySource,
        filters: Vec<Filter>,
        notes: Vec<NoteRef>,
    ) -> Self {
        let mut timeline = Timeline::new(
            TimelineKind::profile(source),
            FilterState::ready(filters),
            TimelineTab::full_tabs(),
        );

        copy_notes_into_timeline(&mut timeline, txn, ndb, note_cache, notes);

        Profile {
            timeline,
            multi_subscriber: None,
        }
    }

    fn filters_raw(pk: &[u8; 32]) -> Vec<FilterBuilder> {
        vec![Filter::new()
            .authors([pk])
            .kinds([1])
            .limit(default_limit())]
    }
}

impl NotesHolder for Profile {
    fn get_multi_subscriber(&mut self) -> Option<&mut MultiSubscriber> {
        self.multi_subscriber.as_mut()
    }

    fn get_view(&mut self) -> &mut crate::timeline::TimelineTab {
        self.timeline.current_view_mut()
    }

    fn filters(for_id: &[u8; 32]) -> Vec<enostr::Filter> {
        Profile::filters_raw(for_id)
            .into_iter()
            .map(|mut f| f.build())
            .collect()
    }

    fn filters_since(for_id: &[u8; 32], since: u64) -> Vec<enostr::Filter> {
        Profile::filters_raw(for_id)
            .into_iter()
            .map(|f| f.since(since).build())
            .collect()
    }

    fn new_notes_holder(
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        id: &[u8; 32],
        filters: Vec<Filter>,
        notes: Vec<NoteRef>,
    ) -> Self {
        Profile::new(
            txn,
            ndb,
            note_cache,
            PubkeySource::Explicit(Pubkey::new(*id)),
            filters,
            notes,
        )
    }

    fn set_multi_subscriber(&mut self, subscriber: MultiSubscriber) {
        self.multi_subscriber = Some(subscriber);
    }
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

                let _ = ndb.process_client_event(raw_msg.as_str());
                let _ = state_map.remove_entry(&changes.kp.pubkey);

                info!("sending {}", raw_msg);
                pool.send(&enostr::ClientMessage::raw(raw_msg));

                router.go_back();
            }
        }
    }
}
