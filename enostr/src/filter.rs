use crate::{NoteId, Pubkey};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct Filter {
    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<NoteId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authors: Option<Vec<Pubkey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kinds: Option<Vec<u64>>,
    #[serde(rename = "#e")]
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Vec<NoteId>>,
    #[serde(rename = "#p")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pubkeys: Option<Vec<Pubkey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<u64>, // unix timestamp seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<u64>, // unix timestamp seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u16>,
}

impl Filter {
    pub fn new() -> Filter {
        Filter {
            ids: None,
            authors: None,
            kinds: None,
            events: None,
            pubkeys: None,
            since: None,
            until: None,
            limit: None,
        }
    }

    pub fn ids(mut self, ids: Vec<NoteId>) -> Self {
        self.ids = Some(ids);
        self
    }

    pub fn authors(mut self, authors: Vec<Pubkey>) -> Self {
        self.authors = Some(authors);
        self
    }

    pub fn kinds(mut self, kinds: Vec<u64>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    pub fn events(mut self, events: Vec<NoteId>) -> Self {
        self.events = Some(events);
        self
    }

    pub fn pubkeys(mut self, pubkeys: Vec<Pubkey>) -> Self {
        self.pubkeys = Some(pubkeys);
        self
    }

    pub fn since(mut self, since: u64) -> Self {
        self.since = Some(since);
        self
    }

    pub fn until(mut self, until: u64) -> Self {
        self.until = Some(until);
        self
    }

    pub fn limit(mut self, limit: u16) -> Self {
        self.limit = Some(limit);
        self
    }
}
