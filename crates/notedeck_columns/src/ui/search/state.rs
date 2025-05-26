use crate::timeline::TimelineTab;
use notedeck::debouncer::Debouncer;
use std::time::Duration;

use super::SearchType;

#[derive(Debug, Eq, PartialEq)]
pub enum SearchState {
    Typing(TypingType),
    PerformSearch(SearchType),
    Searched,
    Navigating,
    New,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TypingType {
    Mention(String),
    AutoSearch,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum FocusState {
    /// Get ready to focus
    Navigating,

    /// We should request focus when we stop navigating
    ShouldRequestFocus,

    /// We already focused, we don't need to do that again
    RequestedFocus,
}

/// Search query state that exists between frames
#[derive(Debug)]
pub struct SearchQueryState {
    /// This holds our search query while we're updating it
    pub string: String,

    /// When the debouncer timer elapses, we execute the search and mark
    /// our state as searchd. This will make sure we don't try to search
    /// again next frames
    pub state: SearchState,

    /// A bit of context to know if we're navigating to the view. We
    /// can use this to know when to request focus on the textedit
    pub focus_state: FocusState,

    /// When was the input updated? We use this to debounce searches
    pub debouncer: Debouncer,

    /// The search results
    pub notes: TimelineTab,
}

impl Default for SearchQueryState {
    fn default() -> Self {
        SearchQueryState::new()
    }
}

impl SearchQueryState {
    pub fn new() -> Self {
        Self {
            string: "".to_string(),
            state: SearchState::New,
            notes: TimelineTab::default(),
            focus_state: FocusState::Navigating,
            debouncer: Debouncer::new(Duration::from_millis(200)),
        }
    }
}
