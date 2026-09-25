//! The chrome's app roster: adding apps, the opened-app bitset, finding and
//! switching to a specific app, and closing / cycling the opened app tabs.

use super::{Chrome, MAX_APPS};
use crate::app::NotedeckApp;
use notedeck_columns::Damus;
use oot_bitset::{bitset_clear, bitset_get, bitset_set};

#[cfg(feature = "dave")]
use notedeck_dave::Dave;

impl Chrome {
    pub fn add_app(&mut self, app: NotedeckApp) {
        self.apps.push(app);
        // `opened` is a fixed bitset — the new app's bit is already clear.
    }

    /// Whether the app at index `i` has been opened.
    pub(super) fn is_opened(&self, i: usize) -> bool {
        i < MAX_APPS && bitset_get(&self.opened, i as u16)
    }

    /// Mark the app at index `i` as opened.
    pub(super) fn set_opened(&mut self, i: usize) {
        if i < MAX_APPS {
            bitset_set(&mut self.opened, i as u16);
        }
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

    /// Close the active app's tab and switch to the nearest opened app.
    /// No-op if it's the only opened app — at least one app must stay open.
    /// Used by Ctrl+W.
    pub(super) fn close_active_app(&mut self) {
        let n = self.apps.len();
        if n == 0 {
            return;
        }
        // at least one app must remain open
        if self.opened_count() <= 1 {
            return;
        }
        let active = self.active.clamp(0, n as i32 - 1) as usize;
        self.clear_opened(active);
        // switch to the nearest opened app after `active`, wrapping
        for offset in 1..=n {
            let idx = (active + offset) % n;
            if self.is_opened(idx) {
                self.set_active(idx as i32);
                return;
            }
        }
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
