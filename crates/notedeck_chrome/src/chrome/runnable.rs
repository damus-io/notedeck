//! What a person can run now, for a command bar: every registered tool that
//! would do something if called this moment, with its keys.
//!
//! The registry ([`notedeck::ToolRegistry`]) lists every running app's tools
//! and the chrome's own. [`Chrome::runnable_tools`] narrows that to the ones
//! that are available, by the same tests a key binding uses
//! ([`Chrome::key_tool_available`]), and orders them for listing: the active
//! app's first, then the chrome's, then the other running apps'.

use notedeck::{App, ToolEntry, ToolKind, ToolOwner, ToolRegistry, ToolSpec};

use super::Chrome;
use crate::keymap::{KeySeq, Scopes};

/// Where a [`RunnableTool`] comes from, in the order a command bar lists
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolPlace {
    /// The app in front.
    Active,
    /// The chrome itself.
    Shell,
    /// A running app behind the one in front.
    Background,
}

/// A tool that would do something if run now, as a command bar lists it.
#[derive(Debug, Clone)]
pub struct RunnableTool {
    /// Its spec: name, [title](ToolSpec::display_title),
    /// [category](ToolSpec::category), args.
    pub spec: ToolSpec,
    /// Who runs it: the shell, or the app that owns it.
    pub owner: ToolOwner,
    /// Where it sorts.
    pub place: ToolPlace,
    /// The keys that run it as the bar would, in the active app's scopes
    /// ([`Keymap::keys_for`](crate::keymap::Keymap::keys_for)).
    pub keys: Option<KeySeq>,
}

impl Chrome {
    /// Every registered tool in `tools` that would do something if run now,
    /// active app's first, then the chrome's, then the other running apps',
    /// each group in registry order.
    ///
    /// A UI tool is listed when it is
    /// [available](Self::ui_tool_available). A data tool is listed when its
    /// app is running, and one that [needs a current ref](ToolSpec::needs_current)
    /// only when its app is in front and has one, since that is what fills the
    /// argument. Each carries the keys that run it in the active app's scopes,
    /// so a tool of an app behind the one in front usually has none.
    ///
    /// Allocates the list: call it when a command bar opens or the running
    /// apps change, not per frame.
    pub fn runnable_tools(&self, tools: &ToolRegistry) -> Vec<RunnableTool> {
        let active = self.apps.get(self.active as usize);
        let current = active.and_then(|app| app.current_ref());
        let scopes =
            Scopes::new(active.map_or(&[][..], |app| app.key_scopes())).with_current(current);
        let keymap = self.keymap();

        let mut runnable: Vec<RunnableTool> = tools
            .entries()
            .filter_map(|entry| {
                let owner = entry.owner?;
                let place = self.place_of(owner);
                self.can_run(tools, entry, place, current.is_some())
                    .then(|| RunnableTool {
                        spec: entry.spec.clone(),
                        owner,
                        place,
                        keys: keymap.keys_for(entry.spec.name(), scopes),
                    })
            })
            .collect();
        // Stable, so each place keeps registry order.
        runnable.sort_by_key(|tool| tool.place);
        runnable
    }

    /// Where a tool `owner` runs sorts in a command bar.
    fn place_of(&self, owner: ToolOwner) -> ToolPlace {
        match owner {
            ToolOwner::Shell => ToolPlace::Shell,
            ToolOwner::App(id) if id.slot() as i32 == self.active => ToolPlace::Active,
            ToolOwner::App(_) => ToolPlace::Background,
        }
    }

    /// Whether `entry`, sorting at `place`, would do something if run now,
    /// `has_current` saying whether the app in front has a current ref.
    fn can_run(
        &self,
        tools: &ToolRegistry,
        entry: ToolEntry<'_>,
        place: ToolPlace,
        has_current: bool,
    ) -> bool {
        let name = entry.spec.name();
        match entry.spec.kind() {
            ToolKind::Ui => self.ui_tool_available(tools, name),
            ToolKind::Data if entry.spec.needs_current() => {
                place == ToolPlace::Active && has_current
            }
            ToolKind::Data => match entry.owner {
                Some(ToolOwner::App(id)) => self.is_running(id.slot()),
                _ => false,
            },
        }
    }
}
