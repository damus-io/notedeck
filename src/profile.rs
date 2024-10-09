use enostr::Filter;
use nostrdb::{FilterBuilder, ProfileRecord};

use crate::{
    filter,
    multi_subscriber::MultiSubscriber,
    note::NoteRef,
    notes_holder::NotesHolder,
    timeline::{Timeline, TimelineTab, ViewFilter},
};

pub enum DisplayName<'a> {
    One(&'a str),

    Both {
        username: &'a str,
        display_name: &'a str,
    },
}

impl<'a> DisplayName<'a> {
    pub fn username(&self) -> &'a str {
        match self {
            Self::One(n) => n,
            Self::Both { username, .. } => username,
        }
    }
}

fn is_empty(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}

pub fn get_profile_name<'a>(record: &'a ProfileRecord) -> Option<DisplayName<'a>> {
    let profile = record.record().profile()?;
    let display_name = profile.display_name().filter(|n| !is_empty(n));
    let name = profile.name().filter(|n| !is_empty(n));

    match (display_name, name) {
        (None, None) => None,
        (Some(disp), None) => Some(DisplayName::One(disp)),
        (None, Some(username)) => Some(DisplayName::One(username)),
        (Some(display_name), Some(username)) => Some(DisplayName::Both {
            display_name,
            username,
        }),
    }
}

pub struct Profile {
    view: TimelineTab,
    pub multi_subscriber: Option<MultiSubscriber>,
}

impl Profile {
    pub fn new(notes: Vec<NoteRef>) -> Self {
        let mut cap = ((notes.len() as f32) * 1.5) as usize;
        if cap == 0 {
            cap = 25;
        }
        let mut view = TimelineTab::new_with_capacity(ViewFilter::NotesAndReplies, cap);
        view.notes = notes;

        Profile {
            view,
            multi_subscriber: None,
        }
    }

    fn filters_raw(pk: &[u8; 32]) -> Vec<FilterBuilder> {
        vec![Filter::new()
            .authors([pk])
            .kinds([1])
            .limit(filter::default_limit())]
    }
}

impl NotesHolder for Profile {
    fn get_multi_subscriber(&mut self) -> Option<&mut MultiSubscriber> {
        self.multi_subscriber.as_mut()
    }

    fn get_view(&mut self) -> &mut crate::timeline::TimelineTab {
        &mut self.view
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

    fn new_notes_holder(notes: Vec<NoteRef>) -> Self {
        Profile::new(notes)
    }
}
