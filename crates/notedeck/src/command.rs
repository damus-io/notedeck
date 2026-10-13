//! One row of a command bar: what it says, and what choosing it does.
//!
//! The chrome's Cmd-K bar lists these. It filters them with
//! [`fuzzy_match`](crate::fuzzy::fuzzy_match) on their titles and groups them
//! by section. The rows come from the registered tools and from each app's
//! search results; the bar itself only draws and runs them.

use std::borrow::Cow;

use crate::{AppId, OpenUri, ToolInvocation};

/// One row of a command bar.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandItem {
    /// The heading it is listed under (`Headway`, `Dave sessions`). Rows of
    /// one section are listed together, in the order their best row ranks.
    /// An empty one has no heading drawn.
    pub section: Cow<'static, str>,
    /// What it is or does, as a person reads it. The query matches this.
    pub title: String,
    /// More about it, drawn muted after the title. Not matched.
    pub subtitle: Option<String>,
    /// More words the query matches but nobody sees, like a tool's name and
    /// description. A row matched only here ranks below every row whose
    /// title matched, and each query word must appear whole
    /// ([`contains_words`](crate::fuzzy::contains_words)).
    pub keywords: Option<String>,
    /// The keys that run it outside the bar, as a keycap (`gb`, `^w j`).
    pub keycap: Option<String>,
    /// Set on a search hit, which its provider already matched to the query
    /// some way of its own: the score it matched at. The bar then keeps the
    /// row whatever its title matches, ranked by its title's match when the
    /// query matches it and by this when it doesn't.
    pub found: Option<i32>,
    /// What choosing it does.
    pub action: CommandAction,
}

impl CommandItem {
    /// A row titled `title` under `section` that does `action`, with no
    /// subtitle or keycap.
    pub fn new(
        section: impl Into<Cow<'static, str>>,
        title: impl Into<String>,
        action: CommandAction,
    ) -> Self {
        Self {
            section: section.into(),
            title: title.into(),
            subtitle: None,
            keywords: None,
            keycap: None,
            found: None,
            action,
        }
    }

    /// This row with `subtitle` drawn after its title.
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// This row matched by `keywords` too (see [`CommandItem::keywords`]).
    pub fn with_keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = Some(keywords.into());
        self
    }

    /// This row as a search hit its provider matched at `score` (see
    /// [`CommandItem::found`]).
    pub fn found(mut self, score: i32) -> Self {
        self.found = Some(score);
        self
    }

    /// This row with `keycap` drawn at its right edge.
    pub fn with_keycap(mut self, keycap: impl Into<String>) -> Self {
        self.keycap = Some(keycap.into());
        self
    }
}

/// What choosing a [`CommandItem`] does.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandAction {
    /// Run a tool, as a key bound to it would: a UI tool reaches the app that
    /// owns it, a data tool runs at once.
    RunTool(ToolInvocation),
    /// Open the entity a reference names (`headway:board/word-id`), as a click
    /// on an inline reference does.
    Open(OpenUri),
    /// Switch to the app in this slot of the chrome's roster.
    SwitchApp(AppId),
}
