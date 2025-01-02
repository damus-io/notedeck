use enostr::{Filter, Pubkey};
use nostrdb::{FilterBuilder, Ndb, ProfileRecord, Transaction};

use notedeck::{filter::default_limit, FilterState, MuteFun, NoteCache, NoteRef};

use crate::{
    multi_subscriber::MultiSubscriber,
    notes_holder::NotesHolder,
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
        is_muted: &MuteFun,
    ) -> Self {
        let mut timeline = Timeline::new(
            TimelineKind::profile(source),
            FilterState::ready(filters),
            TimelineTab::full_tabs(),
        );

        copy_notes_into_timeline(&mut timeline, txn, ndb, note_cache, notes, is_muted);

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
        is_muted: &MuteFun,
    ) -> Self {
        Profile::new(
            txn,
            ndb,
            note_cache,
            PubkeySource::Explicit(Pubkey::new(*id)),
            filters,
            notes,
            is_muted,
        )
    }

    fn set_multi_subscriber(&mut self, subscriber: MultiSubscriber) {
        self.multi_subscriber = Some(subscriber);
    }
}
