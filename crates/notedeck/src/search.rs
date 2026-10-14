//! Universal search: what an app hands the chrome's Cmd-K bar for a query.
//!
//! Each app answers a query through one hook,
//! [`App::search`](crate::App::search), by pushing [`SearchHit`]s into a
//! [`SearchHits`]. The chrome lists them in the bar under the app's name,
//! beside the tools, and opens a chosen hit through
//! [`AppAction::Open`](crate::AppAction::Open) on its [`uri`](SearchHit::uri),
//! the same way a click on an inline reference opens it.
//!
//! ## The contract
//!
//! - The chrome asks when the query changes, never per frame, and only for a
//!   query with something in it: the empty bar lists tools alone.
//! - A provider stops at [`SearchHits::is_full`]; a push past the cap is
//!   dropped. The cap is per provider, so one app can't crowd out the rest.
//! - A provider answers at once with what it has. One whose answer takes slow
//!   I/O (a full-text query on a worker) marks the answer
//!   [`pending`](SearchHits::set_pending) and starts the work; the chrome then
//!   asks it again every [`REFINE_INTERVAL`] while the query stands, replacing
//!   its hits each time, until an answer comes back not pending. A provider
//!   must not block for the work, and must drop or ignore the work of a query
//!   it is no longer asked about.

use std::time::Duration;

use nostrdb_net::{NoteId, Pubkey};

use crate::OpenUri;

/// How many hits one provider may give for a query.
pub const HITS_PER_PROVIDER: usize = 8;

/// How long the chrome waits before asking a provider whose answer was
/// [pending](SearchHits::set_pending) again.
pub const REFINE_INTERVAL: Duration = Duration::from_millis(100);

/// What the user typed into the bar, for a provider to answer.
#[derive(Debug, Clone, Copy)]
pub struct SearchQuery<'a> {
    /// The query, trimmed; never empty.
    pub text: &'a str,
}

/// One thing a provider found: a row in the bar that opens what `uri` names.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// What it is called, as a person reads it: a card's title, a session's
    /// name. The bar highlights the query's letters in it.
    pub title: String,
    /// More about it, drawn muted after the title.
    pub subtitle: Option<String>,
    /// What sort of thing it is (`Card`, `Board`, `Session`), drawn before
    /// the subtitle, so two hits of one title can be told apart.
    pub kind: Option<String>,
    /// The reference that opens it (`headway:board/word-id`), resolved the
    /// way [`AppAction::Open`](crate::AppAction::Open) resolves any other.
    pub uri: OpenUri,
    /// How well it matched, higher better, on
    /// [`fuzzy_match`](crate::fuzzy::fuzzy_match)'s scale. The bar ranks a
    /// hit whose title the query fuzzy-matches by that match, like any
    /// other row, and one matched some other way (by its body) by this.
    pub score: i32,
    /// What the hit is in nostr terms, for the bar to draw it as that thing
    /// rather than as text alone (see [`HitPreview`]). `None` draws the
    /// title and subtitle as one line.
    pub preview: Option<HitPreview>,
}

/// What a [`SearchHit`] stands for, so the bar can draw it richly. Typed
/// rather than drawn by the provider: the bar resolves it itself and draws
/// it with what every surface uses, so an app gets a rich row by having a
/// [`KindRenderer`](crate::KindRenderer) for its kind, with no search-only
/// renderer to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitPreview {
    /// A note, drawn by the [`KindRenderer`](crate::KindRenderer) registered
    /// for its kind in [`RenderContext::Row`](crate::RenderContext::Row): a
    /// card's live status chip, a note's author and text. A note not in the
    /// local db, or of a kind nothing renders, draws as text.
    Note(NoteId),
    /// A profile, drawn as its picture before the title.
    Profile(Pubkey),
}

impl SearchHit {
    /// A hit titled `title` that opens `uri`, scored `score`.
    pub fn new(title: impl Into<String>, uri: OpenUri, score: i32) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            kind: None,
            uri,
            score,
            preview: None,
        }
    }

    /// This hit drawn as the thing `preview` names (see [`HitPreview`]).
    pub fn previewed(mut self, preview: HitPreview) -> Self {
        self.preview = Some(preview);
        self
    }

    /// This hit with `subtitle` drawn after its title.
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// This hit marked as a `kind` of thing.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }
}

/// Where a provider puts its hits for one query, capped (see the module
/// docs).
#[derive(Debug, Default)]
pub struct SearchHits {
    /// What has been pushed, up to `cap`.
    hits: Vec<SearchHit>,
    /// The most it takes.
    cap: usize,
    /// Whether the answer is still coming.
    pending: bool,
}

impl SearchHits {
    /// An empty answer taking at most `cap` hits.
    pub fn new(cap: usize) -> Self {
        Self {
            hits: Vec::new(),
            cap,
            pending: false,
        }
    }

    /// Add `hit`, unless the answer is full. Returns whether it was kept.
    pub fn push(&mut self, hit: SearchHit) -> bool {
        if self.is_full() {
            return false;
        }
        self.hits.push(hit);
        true
    }

    /// Whether it takes no more: a provider stops looking here.
    pub fn is_full(&self) -> bool {
        self.hits.len() >= self.cap
    }

    /// Mark the answer as still coming, so the chrome asks again after
    /// [`REFINE_INTERVAL`] (see the module docs).
    pub fn set_pending(&mut self) {
        self.pending = true;
    }

    /// Whether the answer is still coming.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// The hits, in the order they were pushed.
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    /// Empty it for the next provider's answer, keeping its buffer and cap.
    pub fn clear(&mut self) {
        self.hits.clear();
        self.pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str) -> SearchHit {
        SearchHit::new(title, OpenUri::new(format!("test:{title}")), 0)
    }

    #[test]
    fn a_push_past_the_cap_is_dropped() {
        let mut out = SearchHits::new(2);
        assert!(out.push(hit("a")));
        assert!(!out.is_full());
        assert!(out.push(hit("b")));
        assert!(out.is_full());
        assert!(!out.push(hit("c")), "past the cap");
        let titles: Vec<_> = out.hits().iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, ["a", "b"]);
    }

    #[test]
    fn clear_forgets_hits_and_pending_but_keeps_the_cap() {
        let mut out = SearchHits::new(1);
        out.push(hit("a"));
        out.set_pending();
        out.clear();
        assert!(out.hits().is_empty());
        assert!(!out.is_pending());
        assert!(out.push(hit("b")));
        assert!(out.is_full());
    }
}
