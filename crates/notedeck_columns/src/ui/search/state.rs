use crate::timeline::TimelineTab;
use enostr::Pubkey;
use notedeck_ui::ProfileSearchResult;

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
}

#[derive(Debug, Clone)]
pub enum RecentSearchItem {
    Query(String),
    Profile { pubkey: Pubkey, query: String },
}

#[derive(Debug, Eq, PartialEq, Clone, Default)]
pub enum FocusState {
    /// Get ready to focus
    Navigating,

    /// We should request focus when we stop navigating
    ShouldRequestFocus,

    /// We already focused, we don't need to do that again
    #[default]
    RequestedFocus,
}

/// Search query state that exists between frames
#[derive(Debug)]
pub struct SearchQueryState {
    /// This holds our search query while we're updating it
    pub string: String,

    /// Current search state
    pub state: SearchState,

    /// A bit of context to know if we're navigating to the view. We
    /// can use this to know when to request focus on the textedit
    pub focus_state: FocusState,

    /// The search results
    pub notes: TimelineTab,

    /// Currently selected item index in search results (-1 = none, 0 = "search posts", 1+ = users)
    pub selected_index: i32,

    /// Cached user search results for the current query
    pub user_results: Vec<Vec<u8>>,

    /// Recent search history (most recent first, max 10)
    pub recent_searches: Vec<RecentSearchItem>,

    /// Cached @mention search results
    pub mention_results: Vec<ProfileSearchResult>,

    /// The query string that produced `mention_results`
    pub last_mention_query: String,
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
            selected_index: -1,
            user_results: Vec::new(),
            recent_searches: Vec::new(),
            mention_results: Vec::new(),
            last_mention_query: String::new(),
        }
    }

    pub fn add_recent_query(&mut self, query: String) {
        if query.is_empty() {
            return;
        }

        let item = RecentSearchItem::Query(query.clone());
        self.recent_searches
            .retain(|s| !matches!(s, RecentSearchItem::Query(q) if q == &query));
        self.recent_searches.insert(0, item);
        self.recent_searches.truncate(10);
    }

    pub fn add_recent_profile(&mut self, pubkey: Pubkey, query: String) {
        if query.is_empty() {
            return;
        }

        let item = RecentSearchItem::Profile {
            pubkey,
            query: query.clone(),
        };
        self.recent_searches.retain(
            |s| !matches!(s, RecentSearchItem::Profile { pubkey: pk, .. } if pk == &pubkey),
        );
        self.recent_searches.insert(0, item);
        self.recent_searches.truncate(10);
    }

    pub fn remove_recent_search(&mut self, index: usize) {
        if index < self.recent_searches.len() {
            self.recent_searches.remove(index);
        }
    }

    pub fn clear_recent_searches(&mut self) {
        self.recent_searches.clear();
    }
}
