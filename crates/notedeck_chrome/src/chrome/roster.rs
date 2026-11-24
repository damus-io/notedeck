//! The chrome's app roster: adding apps, the opened-app bitset, finding and
//! switching to a specific app, and closing / cycling the opened app tabs.

use super::nav::{AppNav, TakenEntry};
use super::{Chrome, MAX_APPS};
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use notedeck_columns::Damus;
use oot_bitset::{bitset_clear, bitset_get, bitset_set};

#[cfg(feature = "dave")]
use notedeck_dave::Dave;

/// The key that closes the active app's tab: Ctrl+W, or Cmd+W on macOS
/// (egui's `COMMAND` is Cmd there). Bound in the chrome's keybindings and
/// named in the tooltip on each tab's x.
pub(super) const CLOSE_TAB_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W);

impl Chrome {
    /// Append `app` to the roster, returning its slot.
    pub fn add_app(&mut self, app: NotedeckApp) -> notedeck::AppId {
        self.apps.push(app);
        self.app_navs.push(AppNav::new());
        // `opened` is a fixed bitset — the new app's bit is already clear.
        notedeck::AppId(self.apps.len() - 1)
    }

    /// Whether the app at index `i` has been opened.
    pub fn is_opened(&self, i: usize) -> bool {
        i < MAX_APPS && bitset_get(&self.opened, i as u16)
    }

    /// The app in `slot`, if there is one.
    pub fn app_mut(&mut self, slot: usize) -> Option<&mut NotedeckApp> {
        self.apps.get_mut(slot)
    }

    /// Whether the app at index `i` is running: opened, or every app is kept
    /// active (`--all-apps-active`). Only running apps get `update()` calls and
    /// contribute UI tools.
    pub(super) fn is_running(&self, i: usize) -> bool {
        self.options.contains(ChromeOptions::AllAppsActive) || self.is_opened(i)
    }

    /// Mark the app at index `i` as opened.
    pub(super) fn set_opened(&mut self, i: usize) {
        if i < MAX_APPS {
            bitset_set(&mut self.opened, i as u16);
        }
    }

    /// Start the app that owns the tool `name` in `tools`, if it isn't
    /// running and a call is meant to start it: any data tool's app, and a UI
    /// tool's that [starts it](notedeck::ToolSpec::starts_app). A stopped
    /// app's other UI tools are only in `tools` for the frame after it
    /// closed, and leave it closed. The app updates from the next
    /// `update()` on, so it starts syncing; the call itself runs at once,
    /// which a tool that reads the db answers. Doesn't allocate.
    pub(super) fn start_tool_owner(&mut self, tools: &notedeck::ToolRegistry, name: &str) {
        let Some(entry) = tools.entry(name) else {
            return;
        };
        let Some(notedeck::ToolOwner::App(owner)) = entry.owner else {
            return;
        };
        let slot = owner.slot();
        if self.is_running(slot) {
            return;
        }
        if entry.spec.kind() == notedeck::ToolKind::Ui && !entry.spec.is_app_starter() {
            return;
        }
        tracing::info!("starting the app in slot {slot} for a call of '{name}'");
        self.set_opened(slot);
    }

    /// Mark the app at index `i` as closed.
    fn clear_opened(&mut self, i: usize) {
        if i < MAX_APPS {
            bitset_clear(&mut self.opened, i as u16);
        }
    }

    /// The number of currently-opened apps.
    fn opened_count(&self) -> usize {
        (0..self.apps.len()).filter(|&i| self.is_opened(i)).count()
    }

    pub(super) fn get_columns_app(&mut self) -> Option<&mut Damus> {
        for app in &mut self.apps {
            if let NotedeckApp::Columns(cols) = app {
                return Some(cols);
            }
        }

        None
    }

    pub(super) fn switch_to_columns(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Columns(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    #[cfg(feature = "dave")]
    pub(super) fn get_dave_app(&mut self) -> Option<&mut Dave> {
        for app in &mut self.apps {
            if let NotedeckApp::Dave(dave) = app {
                return Some(dave);
            }
        }
        None
    }

    #[cfg(feature = "dave")]
    pub(super) fn switch_to_dave(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Dave(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    #[cfg(feature = "messages")]
    pub(super) fn switch_to_messages(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Messages(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    /// The Headway app's slot — its [`AppId`](notedeck::AppId), which Headway itself never
    /// learns — or `None` when it isn't in the roster.
    #[cfg(feature = "headway")]
    pub(super) fn headway_slot(&self) -> Option<usize> {
        self.apps
            .iter()
            .position(|app| matches!(app, NotedeckApp::Headway(_)))
    }

    #[cfg(feature = "notebook")]
    pub(super) fn get_notebook_app(&mut self) -> Option<&mut notedeck_notebook::Notebook> {
        for app in &mut self.apps {
            if let NotedeckApp::Notebook(notebook) = app {
                return Some(notebook);
            }
        }
        None
    }

    #[cfg(feature = "notebook")]
    pub(super) fn switch_to_notebook(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Notebook(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    /// Close the active app's tab; see [`close_app`](Chrome::close_app).
    /// Used by Ctrl+W (Cmd+W on macOS).
    ///
    /// Returns the dropped entries for
    /// [`cleanup_entries`](Chrome::cleanup_entries).
    #[must_use = "a dropped entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn close_active_app(&mut self) -> Vec<TakenEntry> {
        let Some(last) = self.apps.len().checked_sub(1) else {
            return Vec::new();
        };
        self.close_app(self.active.clamp(0, last as i32) as usize)
    }

    /// Close the tab of the opened app in slot `idx` and drop its nav stack
    /// (see [`drop_app_nav`](Chrome::drop_app_nav)). When `idx` was the
    /// active app, switch to the nearest opened app after it, wrapping;
    /// closing any other tab leaves the active app alone. No-op if `idx`
    /// isn't opened or is the only opened app — at least one app must stay
    /// open. Used by Ctrl+W (through
    /// [`close_active_app`](Chrome::close_active_app)) and by the x on each
    /// tab in the header strip.
    ///
    /// Returns the dropped entries for
    /// [`cleanup_entries`](Chrome::cleanup_entries).
    #[must_use = "a dropped entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn close_app(&mut self, idx: usize) -> Vec<TakenEntry> {
        let n = self.apps.len();
        if idx >= n || !self.is_opened(idx) {
            return Vec::new();
        }
        // at least one app must remain open
        if self.opened_count() <= 1 {
            return Vec::new();
        }
        let was_active = self.active == idx as i32;
        self.clear_opened(idx);
        let dropped = self.drop_app_nav(idx);
        if !was_active {
            return dropped;
        }
        // switch to the nearest opened app after `idx`, wrapping
        for offset in 1..=n {
            let next = (idx + offset) % n;
            if self.is_opened(next) {
                self.set_active(next as i32);
                break;
            }
        }
        dropped
    }

    /// Cycle the active app to the next (`forward`) or previous opened app,
    /// wrapping around. Used by Ctrl+Tab / Ctrl+Shift+Tab.
    pub(super) fn cycle_app(&mut self, forward: bool) {
        let n = self.apps.len();
        if n == 0 {
            return;
        }
        let active = self.active.clamp(0, n as i32 - 1) as usize;
        // walk opened slots starting from the one after active, wrapping
        if let Some(idx) = bitset_find(&self.opened, active as u16, forward) {
            self.set_active(idx as i32);
        }
    }
}

/// The next set flag after `flag`, walking forwards (or backwards when
/// `forward` is false) and wrapping around the end of the bitset. `flag` itself
/// is only returned when it is the sole set flag, since the walk starts one step
/// away and comes all the way back. Returns `None` when nothing is set.
fn bitset_find<const N: usize>(set: &[u16; N], flag: u16, forward: bool) -> Option<u16> {
    let flag_count = (N * 16) as u32;
    let start = u32::from(flag);

    assert!(start < flag_count, "flag is outside the bitset");

    for offset in 1..=flag_count {
        let candidate = if forward {
            (start + offset) % flag_count
        } else {
            (start + flag_count - offset) % flag_count
        } as u16;

        if bitset_get(set, candidate) {
            return Some(candidate);
        }
    }

    None
}

#[cfg(test)]
mod tab_cycle_tests {
    use super::{bitset_find, MAX_APPS};
    use oot_bitset::bitset_set;

    const FORWARD: bool = true;
    const BACK: bool = false;

    fn bitset(flags: &[u16]) -> [u16; MAX_APPS / 16] {
        let mut set = [0u16; MAX_APPS / 16];
        for &f in flags {
            bitset_set(&mut set, f);
        }
        set
    }

    #[test]
    fn cycles_forward_and_wraps() {
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 0, FORWARD), Some(1));
        assert_eq!(bitset_find(&set, 1, FORWARD), Some(6));
        assert_eq!(bitset_find(&set, 6, FORWARD), Some(0));
    }

    #[test]
    fn cycles_backward_and_wraps() {
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 6, BACK), Some(1));
        assert_eq!(bitset_find(&set, 1, BACK), Some(0));
        assert_eq!(bitset_find(&set, 0, BACK), Some(6));
    }

    #[test]
    fn cycling_from_an_unset_flag_still_finds_neighbours() {
        // the active app is always opened, but a stale `active` must not
        // strand the cycle
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 3, FORWARD), Some(6));
        assert_eq!(bitset_find(&set, 3, BACK), Some(1));
    }

    #[test]
    fn lone_flag_cycles_to_itself_and_empty_finds_nothing() {
        assert_eq!(bitset_find(&bitset(&[2]), 2, FORWARD), Some(2));
        assert_eq!(bitset_find(&bitset(&[]), 0, FORWARD), None);
    }
}

#[cfg(test)]
mod close_tab_tests {
    use super::super::nav::Blank;
    use super::Chrome;
    use crate::app::NotedeckApp;

    /// A bare chrome over three stand-in apps, each opened, on slot `active`.
    fn three_open_on(active: usize) -> Chrome {
        let apps = ["a", "b", "c"]
            .into_iter()
            .map(|name| NotedeckApp::Other(name.into(), Box::new(Blank)))
            .collect();
        let mut chrome = Chrome::without_context(apps);
        for slot in 0..3 {
            chrome.set_active(slot);
        }
        chrome.set_active(active as i32);
        chrome
    }

    #[test]
    fn closing_an_inactive_tab_keeps_the_active_app() {
        let mut chrome = three_open_on(0);
        let _dropped = chrome.close_app(2);
        assert!(
            !chrome.is_opened(2),
            "the closed tab's app is no longer opened"
        );
        assert_eq!(chrome.active, 0, "the active app didn't change");
        assert!(chrome.is_opened(0) && chrome.is_opened(1));
    }

    #[test]
    fn closing_the_active_tab_moves_to_the_next_opened() {
        let mut chrome = three_open_on(1);
        let _dropped = chrome.close_app(1);
        assert!(!chrome.is_opened(1));
        assert_eq!(chrome.active, 2, "the next opened app after it");

        // and wraps around the end of the roster
        let _dropped = chrome.close_app(2);
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn closing_the_last_open_tab_is_a_no_op() {
        let mut chrome = three_open_on(0);
        let _dropped = chrome.close_app(1);
        let _dropped = chrome.close_app(2);
        let _dropped = chrome.close_app(0);
        assert!(chrome.is_opened(0), "at least one app stays open");
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn closing_an_unopened_or_missing_slot_is_a_no_op() {
        let mut chrome = three_open_on(0);
        let _dropped = chrome.close_app(1);
        let _dropped = chrome.close_app(1);
        let _dropped = chrome.close_app(9);
        assert!(chrome.is_opened(0) && chrome.is_opened(2));
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn close_active_app_closes_the_active_slot() {
        let mut chrome = three_open_on(2);
        let _dropped = chrome.close_active_app();
        assert!(!chrome.is_opened(2));
        assert_eq!(chrome.active, 0, "Ctrl+W wraps to the first opened app");
    }
}
