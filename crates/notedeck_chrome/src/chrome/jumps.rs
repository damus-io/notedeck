//! Vim's jump list: Ctrl-O back to where a far jump left from, Ctrl-I
//! forward again.
//!
//! The chrome owns the keys and the lists, one per app like vim's per-window
//! list; each app owns what a location is ([`App::jump_location`],
//! [`App::restore_jump`]). A keymap binding marked `jump = true` (`g g`, `G`,
//! `] q`) records the app's location before it fires (see
//! [`Chrome::dispatch_keys`](super::Chrome)), and an app records its own
//! hard-coded jumps with [`AppAction::RecordJump`](notedeck::AppAction).
//!
//! This is not route history: Alt-←/→ walk the active app's nav stack (see
//! `nav.rs`), and a jump never pushes a route. Ctrl-O / Ctrl-I reach that
//! history too once the jump list has nowhere to go: the keymap binds them a
//! second time, to [`NAV_BACK`](super::NAV_BACK) and
//! [`NAV_FORWARD`](super::NAV_FORWARD).

use std::collections::HashMap;

use super::Chrome;
use notedeck::{App, AppContext, AppId, JumpLocation, ToolInvocation, ToolOutcome, ToolSpec};

/// Back to the location before the last jump. Bound to Ctrl-O.
pub const JUMP_BACK: &str = "chrome_jump_back";

/// Forward again, after [`JUMP_BACK`]. Bound to Ctrl-I.
pub const JUMP_FORWARD: &str = "chrome_jump_forward";

/// How many locations one app's list keeps; the oldest goes first. Vim's.
pub const JUMP_LIST_CAP: usize = 100;

/// The jump list's UI tools, for the host's tool registry.
pub(super) fn jump_tools() -> [ToolSpec; 2] {
    [
        ToolSpec::new(
            JUMP_BACK,
            "Go back to where the active app's view was before its last far jump (vim's Ctrl-O).",
            Vec::new(),
        )
        .titled("Jump back"),
        ToolSpec::new(
            JUMP_FORWARD,
            "Go forward again along the active app's jump list (vim's Ctrl-I).",
            Vec::new(),
        )
        .titled("Jump forward"),
    ]
}

/// One app's jump list: the locations jumped from, oldest first, and where
/// along them Ctrl-O and Ctrl-I have walked.
#[derive(Default)]
pub struct JumpList {
    /// Locations, oldest first. No two neighbours are the same.
    entries: Vec<JumpLocation>,
    /// The entry the view is at, or `entries.len()` at the tip: past the
    /// newest, where every new jump lands.
    pos: usize,
}

impl JumpList {
    /// Record `from`, the location a jump is leaving: drop whatever is
    /// ahead of the entry Ctrl-O walked back to (a new jump forgets the
    /// forward part, but keeps the entry it came back to), append it unless
    /// it is the newest already, and return to the tip.
    pub fn push(&mut self, from: JumpLocation) {
        self.entries.truncate(self.pos.saturating_add(1));
        self.append(from);
        self.pos = self.entries.len();
    }

    /// Step back from `here`, the view's location now, returning where to
    /// go. Leaving the tip records `here` first, so Ctrl-I can come back to
    /// it. An entry that is `here` already is stepped over, so a jump that
    /// went nowhere doesn't cost a key press. `None` at the oldest.
    pub fn back(&mut self, here: Option<JumpLocation>) -> Option<JumpLocation> {
        if self.pos >= self.entries.len() {
            if let Some(here) = here.clone() {
                self.append(here);
            }
            self.pos = self.entries.len();
        }
        let mut at = self.pos;
        while at > 0 {
            at -= 1;
            let entry = &self.entries[at];
            if here.as_ref().is_some_and(|here| here.same(&**entry)) {
                continue;
            }
            self.pos = at;
            return Some(entry.clone());
        }
        None
    }

    /// Step forward again, after [`back`](Self::back), returning where to
    /// go. `None` when nothing is ahead.
    pub fn forward(&mut self) -> Option<JumpLocation> {
        let next = self.pos + 1;
        let entry = self.entries.get(next)?.clone();
        self.pos = next;
        Some(entry)
    }

    /// Whether [`back`](Self::back) could go anywhere. It may still find
    /// only the location the view is at. Doesn't allocate.
    pub fn can_go_back(&self) -> bool {
        self.pos > 0
    }

    /// Whether [`forward`](Self::forward) would go anywhere. Doesn't
    /// allocate.
    pub fn can_go_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }

    /// How many locations it holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append `location` unless it is the newest, dropping the oldest past
    /// [`JUMP_LIST_CAP`].
    fn append(&mut self, location: JumpLocation) {
        if self
            .entries
            .last()
            .is_some_and(|last| last.same(&*location))
        {
            return;
        }
        if self.entries.len() == JUMP_LIST_CAP {
            self.entries.remove(0);
        }
        self.entries.push(location);
    }
}

/// Every app's [`JumpList`], by app. Owned by the chrome.
#[derive(Default)]
pub struct JumpLists {
    lists: HashMap<AppId, JumpList>,
}

impl JumpLists {
    /// `app`'s list, if it has jumped yet.
    pub fn get(&self, app: AppId) -> Option<&JumpList> {
        self.lists.get(&app)
    }

    /// `app`'s list, made on its first jump.
    fn get_mut(&mut self, app: AppId) -> &mut JumpList {
        self.lists.entry(app).or_default()
    }
}

impl Chrome {
    /// The app in front, as the jump lists key it.
    fn active_app_id(&self) -> Option<AppId> {
        usize::try_from(self.active).ok().map(AppId)
    }

    /// The jump list of the app in front, if it has jumped yet.
    pub fn active_jump_list(&self) -> Option<&JumpList> {
        self.jumps.get(self.active_app_id()?)
    }

    /// A jump binding is about to fire: record where the app in front is,
    /// if it says.
    pub(super) fn record_active_jump(&mut self) {
        let Some(app) = self.active_app_id() else {
            return;
        };
        let Some(here) = self
            .apps
            .get(app.0)
            .and_then(|active| active.jump_location())
        else {
            return;
        };
        self.jumps.get_mut(app).push(here);
    }

    /// An app recorded its own jump ([`AppAction::RecordJump`](notedeck::AppAction)):
    /// it is the one in front, since only it renders.
    pub(super) fn record_jump_from(&mut self, from: JumpLocation) {
        if let Some(app) = self.active_app_id() {
            self.jumps.get_mut(app).push(from);
        }
    }

    /// Run [`JUMP_BACK`] or [`JUMP_FORWARD`], or `None` for another tool.
    pub(super) fn call_jump_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        let back = match invocation.name.as_str() {
            JUMP_BACK => true,
            JUMP_FORWARD => false,
            _ => return None,
        };
        let Some(app) = self.active_app_id() else {
            return Some(ToolOutcome::Error("no app is active".to_owned()));
        };
        let Some(active) = self.apps.get_mut(app.0) else {
            return Some(ToolOutcome::Error("no app is active".to_owned()));
        };
        let list = self.jumps.get_mut(app);
        let to = if back {
            list.back(active.jump_location())
        } else {
            list.forward()
        };
        let moved = to.is_some();
        if let Some(to) = to {
            active.restore_jump(ctx, &*to);
        }
        Some(ToolOutcome::Data(serde_json::json!({ "moved": moved })))
    }

    /// Whether [`JUMP_BACK`] or [`JUMP_FORWARD`] has anywhere to go in the
    /// app in front, or `None` for another tool. Doesn't allocate.
    pub(super) fn jump_tool_available(&self, name: &str) -> Option<bool> {
        let list = || self.active_jump_list();
        match name {
            JUMP_BACK => Some(list().is_some_and(JumpList::can_go_back)),
            JUMP_FORWARD => Some(list().is_some_and(JumpList::can_go_forward)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn at(n: u32) -> JumpLocation {
        Rc::new(n)
    }

    fn val(location: Option<JumpLocation>) -> Option<u32> {
        location.and_then(|l| l.as_any().downcast_ref::<u32>().copied())
    }

    /// A jump from 1 to 9, then Ctrl-O back to 1 and Ctrl-I to 9 again.
    #[test]
    fn back_records_here_so_forward_returns() {
        let mut list = JumpList::default();
        list.push(at(1));
        assert!(list.can_go_back());
        assert!(!list.can_go_forward());

        assert_eq!(val(list.back(Some(at(9)))), Some(1));
        assert!(list.can_go_forward());
        assert_eq!(val(list.forward()), Some(9));
        assert_eq!(val(list.forward()), None, "nothing past the newest");
        assert_eq!(val(list.back(Some(at(9)))), Some(1));
        assert_eq!(val(list.back(Some(at(1)))), None, "at the oldest");
    }

    /// Back twice, then a new jump: the forward part is gone.
    #[test]
    fn a_new_jump_truncates_the_forward_part() {
        let mut list = JumpList::default();
        list.push(at(1));
        list.push(at(2));
        list.push(at(3));
        assert_eq!(val(list.back(Some(at(4)))), Some(3));
        assert_eq!(val(list.back(Some(at(3)))), Some(2));

        // From 2, a jump: 3 and 4 were ahead, and are forgotten.
        list.push(at(2));
        assert!(!list.can_go_forward());
        assert_eq!(list.len(), 2, "1, 2");
        assert_eq!(val(list.back(Some(at(7)))), Some(2));
        assert_eq!(val(list.back(Some(at(2)))), Some(1));
    }

    /// The same location twice in a row is kept once, and Ctrl-O steps over
    /// an entry that is where the view already is.
    #[test]
    fn duplicates_are_dropped_and_stepped_over() {
        let mut list = JumpList::default();
        list.push(at(1));
        list.push(at(1));
        assert_eq!(list.len(), 1);

        list.push(at(5));
        // `G` from 5 went nowhere (already at the bottom): here is 5.
        assert_eq!(val(list.back(Some(at(5)))), Some(1));
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let mut list = JumpList::default();
        for n in 0..(JUMP_LIST_CAP as u32 + 10) {
            list.push(at(n));
        }
        assert_eq!(list.len(), JUMP_LIST_CAP);
        let mut oldest = None;
        let mut here = None;
        while let Some(to) = list.back(here.take()) {
            oldest = val(Some(to.clone()));
            here = Some(to);
        }
        assert_eq!(oldest, Some(10), "the first ten fell off");
    }

    #[test]
    fn an_app_with_no_location_still_walks_back() {
        let mut list = JumpList::default();
        list.push(at(1));
        assert_eq!(val(list.back(None)), Some(1));
        assert!(!list.can_go_forward(), "nothing recorded to come back to");
    }
}
