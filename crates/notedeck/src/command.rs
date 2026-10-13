//! One row of a command bar: what it says, and what choosing it does.
//!
//! The chrome's Cmd-K bar lists these. It filters them with
//! [`fuzzy_match`](crate::fuzzy::fuzzy_match) on their titles and groups them
//! by section. The rows come from the registered tools and from each app's
//! search results; the bar itself only draws and runs them.

use std::borrow::Cow;

use crate::{OpenUri, ToolInvocation};

/// One row of a command bar.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandItem {
    /// The heading it is listed under (`Headway`, `Dave sessions`). Rows of
    /// one section are listed together, in the order their best row ranks.
    pub section: Cow<'static, str>,
    /// What it is or does, as a person reads it. The query matches this.
    pub title: String,
    /// More about it, drawn muted after the title. Not matched.
    pub subtitle: Option<String>,
    /// The keys that run it outside the bar, as a keycap (`gb`, `^w j`).
    pub keycap: Option<String>,
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
            keycap: None,
            action,
        }
    }

    /// This row with `subtitle` drawn after its title.
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
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
}
