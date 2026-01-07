//! Publication state management for NKBIP-01 publications (kind 30040/30041)
//!
//! Similar to thread.rs but handles the hierarchical structure of publications
//! where a 30040 index references multiple 30041 content sections via a-tags.

use enostr::{NoteId, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::subscriptions;

/// A content section from a publication (kind 30041)
#[derive(Debug, Clone)]
pub struct PublicationSection {
    /// The dtag identifier for this section
    pub dtag: String,
    /// The NoteKey if we have fetched this section
    pub note_key: Option<NoteKey>,
    /// The title from the title tag
    pub title: Option<String>,
}

impl PublicationSection {
    pub fn new(dtag: String) -> Self {
        Self {
            dtag,
            note_key: None,
            title: None,
        }
    }

    pub fn with_note(mut self, key: NoteKey, title: Option<String>) -> Self {
        self.note_key = Some(key);
        self.title = title;
        self
    }
}

/// State for a single publication being viewed
#[derive(Debug)]
pub struct PublicationNode {
    /// The index note key (kind 30040)
    pub index_key: NoteKey,
    /// Sections referenced by a-tags, in order
    pub sections: Vec<PublicationSection>,
    /// Remote subscription ID for fetching sections
    pub sub_id: Option<String>,
    /// Whether we've completed fetching all sections
    pub fetch_complete: bool,
}

impl PublicationNode {
    pub fn new(index_key: NoteKey) -> Self {
        Self {
            index_key,
            sections: Vec::new(),
            sub_id: None,
            fetch_complete: false,
        }
    }
}

/// Manages all publication views, similar to Threads
#[derive(Default)]
pub struct Publications {
    /// Map from index NoteId to publication state
    pub publications: HashMap<NoteId, PublicationNode>,
}

impl Publications {
    /// Open a publication for viewing
    pub fn open(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        txn: &Transaction,
        index_id: &NoteId,
    ) -> Option<&mut PublicationNode> {
        // Check if already open
        if self.publications.contains_key(index_id) {
            return self.publications.get_mut(index_id);
        }

        // Get the index note
        let index_note = ndb.get_note_by_id(txn, index_id.bytes()).ok()?;
        let index_key = index_note.key()?;

        // Parse sections from a-tags
        let sections = parse_sections(&index_note);
        info!(
            "Opening publication with {} sections",
            sections.len()
        );

        // Create the node
        let mut node = PublicationNode::new(index_key);
        node.sections = sections;

        // Subscribe to fetch sections if we have any
        if !node.sections.is_empty() {
            let filter = build_sections_filter(&index_note, &node.sections);
            if !filter.is_empty() {
                let sub_id = subscriptions::new_sub_id();
                debug!("Subscribing for {} section filters", filter.len());
                pool.subscribe(sub_id.clone(), filter);
                node.sub_id = Some(sub_id);
            }
        }

        self.publications.insert(*index_id, node);
        self.publications.get_mut(index_id)
    }

    /// Poll for newly fetched section content
    pub fn poll_section_notes(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        index_id: &NoteId,
    ) -> bool {
        let Some(node) = self.publications.get_mut(index_id) else {
            return false;
        };

        if node.fetch_complete {
            return false;
        }

        let mut updated = false;

        // Check each section that doesn't have a note_key yet
        for section in &mut node.sections {
            if section.note_key.is_some() {
                continue;
            }

            // Try to find the section note by querying nostrdb
            // The section should have been indexed by now if received
            if let Some(key) = find_section_note(ndb, txn, &section.dtag) {
                section.note_key = Some(key);
                if let Ok(note) = ndb.get_note_by_key(txn, key) {
                    section.title = get_tag_value(&note, "title").map(String::from);
                }
                updated = true;
                debug!("Found section '{}' with key {:?}", section.dtag, key);
            }
        }

        // Check if all sections are fetched
        if node.sections.iter().all(|s| s.note_key.is_some()) {
            node.fetch_complete = true;
            debug!("All {} sections fetched", node.sections.len());
        }

        updated
    }

    /// Get a publication node
    pub fn get(&self, index_id: &NoteId) -> Option<&PublicationNode> {
        self.publications.get(index_id)
    }

    /// Close a publication view
    pub fn close(&mut self, pool: &mut RelayPool, index_id: &NoteId) {
        if let Some(node) = self.publications.remove(index_id) {
            // Unsubscribe if we have an active subscription
            if let Some(sub_id) = node.sub_id {
                pool.unsubscribe(sub_id);
            }
        }
    }
}

/// Parse section addresses from a-tags in an index note
fn parse_sections(note: &Note) -> Vec<PublicationSection> {
    let mut sections = Vec::new();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(tag_name) = tag.get(0).and_then(|t| t.variant().str()) else {
            continue;
        };

        if tag_name != "a" {
            continue;
        }

        let Some(address) = tag.get(1).and_then(|t| t.variant().str()) else {
            continue;
        };

        // Parse kind:pubkey:dtag format
        let parts: Vec<&str> = address.splitn(3, ':').collect();
        if parts.len() >= 3 {
            let dtag = parts[2].to_string();
            sections.push(PublicationSection::new(dtag));
        }
    }

    sections
}

/// Build nostrdb filters to fetch all sections
fn build_sections_filter(index_note: &Note, sections: &[PublicationSection]) -> Vec<Filter> {
    if sections.is_empty() {
        return vec![];
    }

    let pubkey = index_note.pubkey();

    // Build a filter for each section's d-tag
    // We query for kind 30041 (content) with the specific d-tag
    let dtags: Vec<&str> = sections.iter().map(|s| s.dtag.as_str()).collect();

    // Create filter for all sections at once
    vec![Filter::new()
        .kinds([30041])
        .authors([pubkey])
        .tags(dtags, 'd')
        .build()]
}

/// Find a section note by d-tag in nostrdb
fn find_section_note(ndb: &Ndb, txn: &Transaction, dtag: &str) -> Option<NoteKey> {
    // Query nostrdb for kind 30041 with this d-tag
    let filter = Filter::new()
        .kinds([30041])
        .tags([dtag], 'd')
        .limit(1)
        .build();

    let results = ndb.query(txn, &[filter], 1).ok()?;
    results.first().map(|r| r.note_key)
}

/// Get a tag value by name from a note
fn get_tag_value<'a>(note: &'a Note, tag_name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() >= 2 {
            if let Some(name) = tag.get(0).and_then(|t| t.variant().str()) {
                if name == tag_name {
                    return tag.get(1).and_then(|t| t.variant().str());
                }
            }
        }
    }
    None
}
